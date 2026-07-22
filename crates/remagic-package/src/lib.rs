//! Transactional installer for Store-delivered ReMagic application bundles.
//!
//! Package provenance is established by the signed Store catalog and its
//! artifact digest. This crate then enforces the bundle's complete per-file
//! inventory, compatibility metadata, immutable release layout, and atomic
//! manifest/current publication.

mod bundle;
mod filesystem;
mod manager;
mod state;

pub use bundle::{BundleFileV1, BundleV1, PreparedPackage, PACKAGE_SCHEMA_V1};
pub use manager::{InstallOutcome, PackageError, PackageManager, PackagePaths, UninstallOutcome};
pub use state::InstalledPackageStateV1;
