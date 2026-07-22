use super::*;
use std::fs;
use tracing::warn;

impl Daemon {
    pub(super) async fn rollback_error(&self, context: &LaunchContext, cause: String) -> String {
        match self.rollback_launch(context).await {
            Ok(()) => format!("{cause}; {} launch was rolled back", context.id),
            Err(rollback) => format!(
                "{cause}; {} rollback failed and requires domain recovery: {rollback}",
                context.id
            ),
        }
    }

    async fn rollback_launch(&self, context: &LaunchContext) -> Result<(), String> {
        // Nothing below this fence may erase ownership evidence or claim that
        // Home is authoritative while the failed application cgroup survives.
        if context.background_execution.freezes_process()
            && self.controller.is_active_checked(&context.unit).await?
        {
            self.controller
                .thaw_and_wait(&context.unit)
                .await
                .map_err(|error| {
                    format!(
                        "could not thaw {} during launch rollback: {error}",
                        context.id
                    )
                })?;
        }
        self.controller
            .stop_and_wait(&context.unit)
            .await
            .map_err(|error| format!("could not stop {}: {error}", context.unit))?;
        self.mark_session_process_stopped(&context.id).await?;
        let _ = fs::remove_file(&context.launch_path);
        self.runtime_generations.write().await.remove(&context.id);
        self.runtime_background_execution
            .write()
            .await
            .remove(&context.id);
        self.runtime_foreground_fences
            .write()
            .await
            .remove(&context.id);
        self.runtime_input_modes.write().await.remove(&context.id);
        self.runtime_exit_reports.write().await.remove(&context.id);
        self.runtime_missing_observations
            .write()
            .await
            .remove(&context.id);
        self.state
            .write()
            .await
            .apply(Transition::AppExited(context.id.clone()))
            .map_err(|error| format!("could not roll back launch state: {error}"))?;
        self.show_manager_surface(false)
            .await
            .map_err(|error| format!("could not restore manager surface: {error}"))?;
        let schema_safe = schema::background_restore_is_safe(context);
        if schema_safe {
            self.start_background_service(context)
                .await
                .map_err(|error| format!("could not restore background service: {error}"))?;
        } else {
            warn!(
                app = %context.id,
                generation = context.generation,
                "background service remains stopped because schema completion was not proven"
            );
        }
        Ok(())
    }
}
