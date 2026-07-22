use super::ExecutorError;
use remagic_core::runtime::NetworkEnforcement;
use remagic_core::Capability;
use std::collections::BTreeSet;
use std::env;
use std::path::PathBuf;

pub(super) const DEFAULT_QTFB_SOCKET: &str = "/tmp/qtfb.sock";
pub(super) const DEFAULT_PATH: &str =
    "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
pub(super) const DEFAULT_CAPABILITIES: &[&str] = &[
    "display:qtfb-v1",
    "input:touch-v1",
    "input:pen-v1",
    "input:mode-v2",
    "ink:direct-v1",
    "lifecycle:v2",
    "network:outbound-v1",
];
pub(super) const APPROVED_LIBRARY_DIRS: &[&str] = &[
    "/home/root/apps/remagic/lib",
    "/usr/lib/plugins/scenegraph",
    "/usr/lib",
    "/lib",
];

#[derive(Clone, Debug)]
pub(crate) struct PlatformRuntime {
    pub capabilities: BTreeSet<Capability>,
    pub qtfb_socket: PathBuf,
    pub path: String,
    pub library_search_dirs: Vec<PathBuf>,
    pub zoneinfo_root: PathBuf,
    pub home_root: PathBuf,
    pub runtime_root: PathBuf,
    pub network_enforcement: NetworkEnforcement,
}

impl PlatformRuntime {
    pub fn from_process() -> Result<Self, ExecutorError> {
        let capabilities = platform_capabilities()?;
        let mut library_search_dirs = APPROVED_LIBRARY_DIRS
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        deduplicate_paths(&mut library_search_dirs);
        Ok(Self {
            capabilities,
            qtfb_socket: env::var_os("REMAGIC_QTFB_SOCKET")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_QTFB_SOCKET)),
            path: env::var("PATH").unwrap_or_else(|_| DEFAULT_PATH.to_owned()),
            library_search_dirs,
            zoneinfo_root: PathBuf::from("/usr/share/zoneinfo"),
            home_root: PathBuf::from("/home/root"),
            runtime_root: PathBuf::from("/run/remagic/apps"),
            // The runner currently exposes policy metadata but does not own a
            // network namespace, seccomp filter, or firewall rule set.
            network_enforcement: NetworkEnforcement::MetadataOnly,
        })
    }
}

fn platform_capabilities() -> Result<BTreeSet<Capability>, ExecutorError> {
    match env::var("REMAGIC_PLATFORM_CAPABILITIES") {
        Ok(value) => value
            .split(|character: char| character == ',' || character.is_ascii_whitespace())
            .filter(|value| !value.is_empty())
            .map(|value| Capability::new(value.to_owned()))
            .collect::<Result<_, _>>()
            .map_err(|error| ExecutorError::Policy(error.to_string())),
        Err(_) => Ok(DEFAULT_CAPABILITIES
            .iter()
            .map(|value| Capability::new((*value).to_owned()).expect("constant capability"))
            .collect()),
    }
}

pub(super) fn deduplicate_paths(paths: &mut Vec<PathBuf>) {
    let mut seen = BTreeSet::new();
    paths.retain(|path| seen.insert(path.clone()));
}
