use remagic_core::{AppId, ManifestStore, NetworkMode, RuntimeProfile, MANIFEST_SCHEMA_V2};
use std::path::PathBuf;

#[test]
fn isolated_device_manifests_satisfy_the_production_runtime_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("testing/manifests");
    let manifests = ManifestStore::new(root).load_all().unwrap();
    assert_eq!(manifests.len(), 2);

    for name in ["magicpaper", "koreader"] {
        let id = AppId::new(name).unwrap();
        let manifest = &manifests[&id];
        assert_eq!(manifest.schema, MANIFEST_SCHEMA_V2);
        assert_eq!(manifest.runtime.profile, RuntimeProfile::QtfbCompat);
        assert_eq!(manifest.runtime.network.mode, NetworkMode::Deny);
        let (key, expected) = if name == "magicpaper" {
            ("MAGICPAPER_TEST_MODE", "1")
        } else {
            (
                "KO_HOME",
                "/home/root/.local/state/remagic/acceptance/current/koreader/data",
            )
        };
        assert_eq!(
            manifest.environment.get(key).map(String::as_str),
            Some(expected)
        );
    }
}
