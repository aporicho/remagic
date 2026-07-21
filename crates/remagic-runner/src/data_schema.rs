//! Transactional application-data schema handling.
//!
//! This boundary runs after the schema-v2 environment has been resolved, but
//! before lifecycle endpoints or an application process are created. A
//! durable snapshot plus its pending journal are therefore the only externally
//! visible side effects of a migration which has not yet committed its version
//! record.

use crate::executor::ExecutionPlan;
use remagic_core::{
    AppManifest, DataSchema, LaunchEnvironment, MANIFEST_SCHEMA_V2, SCHEMA_COMPLETE_FILE,
    SCHEMA_PREPARED_FILE,
};
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

mod backup;
mod persistence;
mod process;

use backup::BackupStore;
use persistence::{
    AppliedSchema, PendingMigration, SchemaStateStore, PENDING_FORMAT, STATE_FORMAT,
};

const MANAGED_STATE_DIRECTORY: &str = ".remagic-schema";

/// Construction proof consumed by bootstrap before lifecycle resources exist.
pub(crate) struct SchemaReady(());

pub(crate) fn apply(
    manifest: &AppManifest,
    plan: &ExecutionPlan,
) -> Result<SchemaReady, DataSchemaError> {
    let Some(schema) = manifest.data_schema.as_ref() else {
        return Ok(SchemaReady(()));
    };
    if manifest.schema != MANIFEST_SCHEMA_V2 {
        return Err(DataSchemaError::RequiresManifestV2);
    }
    let environment = plan
        .launch_environment
        .as_ref()
        .ok_or(DataSchemaError::MissingLaunchEnvironment)?;
    environment
        .validate()
        .map_err(|error| DataSchemaError::InvalidLaunchEnvironment(error.to_string()))?;
    let state_root = environment
        .directories
        .state_home
        .join(MANAGED_STATE_DIRECTORY);
    let generation = plan
        .generation
        .filter(|value| *value != 0)
        .ok_or(DataSchemaError::MissingLaunchGeneration)?;
    let phases = SchemaPhases::new(&environment.directories.runtime_dir, generation);
    apply_at_with_phases(manifest, schema, environment, &state_root, Some(&phases))?;
    phases.publish_complete()?;
    Ok(SchemaReady(()))
}

#[cfg(test)]
fn apply_at(
    manifest: &AppManifest,
    schema: &DataSchema,
    environment: &LaunchEnvironment,
    state_root: &Path,
) -> Result<(), DataSchemaError> {
    apply_at_with_phases(manifest, schema, environment, state_root, None)
}

fn apply_at_with_phases(
    manifest: &AppManifest,
    schema: &DataSchema,
    environment: &LaunchEnvironment,
    state_root: &Path,
    phases: Option<&SchemaPhases>,
) -> Result<(), DataSchemaError> {
    reject_recursive_backup(state_root, &schema.backup_paths)?;
    let state = SchemaStateStore::open(state_root, &manifest.id)?;
    let _lock = state.try_lock()?;
    let applied = state.read()?;
    let backups = BackupStore::new(state.backups_root(), manifest.id.clone());

    // A pending journal is the sole authority for crash recovery. Reconcile it
    // before same-version and downgrade decisions so a rolled-back manifest
    // can never launch against data left half migrated by a newer one.
    recover_pending(&state, &backups, applied.as_ref(), state_root)?;

    if let Some(applied) = &applied {
        if applied.version == schema.version {
            state.publish_ready(schema.version)?;
            publish_prepared(phases)?;
            return Ok(());
        }
        if applied.version > schema.version {
            return Err(DataSchemaError::Downgrade {
                applied: applied.version,
                requested: schema.version,
            });
        }
    }

    let from_version = applied.as_ref().map(|value| value.version);
    let snapshot = backups.snapshot(from_version, schema.version, &schema.backup_paths)?;
    // Invalidate the old writer fence before publishing a transaction. A
    // crash may leave the agent disabled, but can never let an older agent
    // write while migration or recovery is unresolved.
    state.clear_ready()?;
    state.publish_pending(&PendingMigration {
        format: PENDING_FORMAT,
        app_id: manifest.id.clone(),
        from_version,
        to_version: schema.version,
        backup: snapshot.name().to_owned(),
        backup_paths: schema.backup_paths.clone(),
    })?;
    publish_prepared(phases)?;

    if let Some(migrator) = &schema.migrator {
        if let Err(migration_error) = process::run_migrator(
            migrator,
            &manifest.working_dir,
            environment,
            from_version,
            schema.version,
            snapshot.path(),
            schema.migration_timeout_ms,
        ) {
            return match snapshot.restore() {
                Ok(()) => match publish_source_ready(&state, from_version)
                    .and_then(|()| state.clear_pending())
                    .and_then(|()| snapshot.retire())
                {
                    Ok(()) => Err(migration_error),
                    Err(cleanup_error) => Err(DataSchemaError::MigrationRestore {
                        migration: migration_error.to_string(),
                        restore: cleanup_error.to_string(),
                    }),
                },
                Err(restore_error) => Err(DataSchemaError::MigrationRestore {
                    migration: migration_error.to_string(),
                    restore: restore_error.to_string(),
                }),
            };
        }
    }

    state.publish(&AppliedSchema {
        format: STATE_FORMAT,
        app_id: manifest.id.clone(),
        version: schema.version,
        backup: snapshot.name().to_owned(),
    })?;
    state.publish_ready(schema.version)?;
    state.clear_pending()?;
    Ok(())
}

fn publish_prepared(phases: Option<&SchemaPhases>) -> Result<(), DataSchemaError> {
    match phases {
        Some(phases) => phases.publish_prepared(),
        None => Ok(()),
    }
}

struct SchemaPhases {
    prepared: PathBuf,
    complete: PathBuf,
    value: Vec<u8>,
}

impl SchemaPhases {
    fn new(runtime_dir: &Path, generation: u64) -> Self {
        Self {
            prepared: runtime_dir.join(SCHEMA_PREPARED_FILE),
            complete: runtime_dir.join(SCHEMA_COMPLETE_FILE),
            value: format!("{generation}\n").into_bytes(),
        }
    }

    fn publish_prepared(&self) -> Result<(), DataSchemaError> {
        persistence::atomic_write(&self.prepared, &self.value)
    }

    fn publish_complete(&self) -> Result<(), DataSchemaError> {
        persistence::atomic_write(&self.complete, &self.value)
    }
}

fn recover_pending(
    state: &SchemaStateStore,
    backups: &BackupStore,
    applied: Option<&AppliedSchema>,
    state_root: &Path,
) -> Result<(), DataSchemaError> {
    let Some(pending) = state.read_pending()? else {
        return Ok(());
    };
    reject_recursive_backup(state_root, &pending.backup_paths)?;
    let snapshot = backups.load_named(&pending.backup)?;
    snapshot.validate_identity(
        pending.from_version,
        pending.to_version,
        &pending.backup_paths,
    )?;
    snapshot.verify()?;

    let committed = applied.is_some_and(|current| {
        current.version == pending.to_version && current.backup == pending.backup
    });
    if committed {
        state.publish_ready(pending.to_version)?;
        return state.clear_pending();
    }

    let source_is_current = match (pending.from_version, applied) {
        (None, None) => true,
        (Some(expected), Some(current)) => current.version == expected,
        _ => false,
    };
    if !source_is_current {
        return Err(DataSchemaError::InvalidState(format!(
            "pending migration {} -> {} is incompatible with the applied schema state",
            pending
                .from_version
                .map_or_else(|| "new".to_owned(), |version| version.to_string()),
            pending.to_version
        )));
    }

    snapshot.restore()?;
    publish_source_ready(state, pending.from_version)?;
    state.clear_pending()?;
    snapshot.retire()
}

fn publish_source_ready(
    state: &SchemaStateStore,
    version: Option<u32>,
) -> Result<(), DataSchemaError> {
    match version {
        Some(version) => state.publish_ready(version),
        None => state.clear_ready(),
    }
}

fn reject_recursive_backup(
    state_root: &Path,
    backup_paths: &[PathBuf],
) -> Result<(), DataSchemaError> {
    for source in backup_paths {
        if state_root.starts_with(source) || source.starts_with(state_root) {
            return Err(DataSchemaError::RecursiveBackup {
                state_root: state_root.to_path_buf(),
                source_path: source.clone(),
            });
        }
    }
    Ok(())
}

#[derive(Debug, Error)]
pub(crate) enum DataSchemaError {
    #[error("data_schema is only executable for a schema-v2 manifest")]
    RequiresManifestV2,
    #[error("schema-v2 data migration has no resolved launch environment")]
    MissingLaunchEnvironment,
    #[error("schema-v2 data migration has no non-zero launch generation")]
    MissingLaunchGeneration,
    #[error("resolved schema-v2 launch environment is invalid: {0}")]
    InvalidLaunchEnvironment(String),
    #[error("data schema downgrade is forbidden: applied={applied}, requested={requested}")]
    Downgrade { applied: u32, requested: u32 },
    #[error(
        "schema state directory {state_root} is recursively inside backup source {source_path}"
    )]
    RecursiveBackup {
        state_root: PathBuf,
        source_path: PathBuf,
    },
    #[error("another data schema transaction is already running")]
    ConcurrentTransaction,
    #[error("invalid persisted data schema state: {0}")]
    InvalidState(String),
    #[error("invalid data backup: {0}")]
    InvalidBackup(String),
    #[error("data backup path changed while it was being copied: {0}")]
    SourceChanged(PathBuf),
    #[error("unsupported object in data backup: {0}")]
    UnsupportedBackupObject(PathBuf),
    #[error("migrator is unavailable or unsafe: {0}")]
    InvalidMigrator(PathBuf),
    #[error("could not start migrator {path}: {source}")]
    StartMigrator { path: PathBuf, source: io::Error },
    #[error("migrator {path} exited unsuccessfully with {status}")]
    MigratorFailed { path: PathBuf, status: String },
    #[error("migrator {path} exceeded its {timeout_ms} ms deadline")]
    MigratorTimedOut { path: PathBuf, timeout_ms: u64 },
    #[error("migration failed ({migration}) and its backup could not be restored ({restore})")]
    MigrationRestore { migration: String, restore: String },
    #[error("{operation} failed for {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot serialize schema transaction metadata: {0}")]
    Json(#[from] serde_json::Error),
}

impl DataSchemaError {
    fn io(operation: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests;
