use super::*;

fn root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "remagic-backlight-{name}-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ))
}

fn write_frontlight(sysfs: &Path, brightness: u32, max_brightness: u32, bl_power: u32) {
    let frontlight = sysfs.join(FRONTLIGHT_PROVIDER);
    fs::create_dir_all(&frontlight).unwrap();
    fs::write(frontlight.join("brightness"), format!("{brightness}\n")).unwrap();
    fs::write(
        frontlight.join("actual_brightness"),
        format!("{brightness}\n"),
    )
    .unwrap();
    fs::write(
        frontlight.join("max_brightness"),
        format!("{max_brightness}\n"),
    )
    .unwrap();
    fs::write(frontlight.join("bl_power"), format!("{bl_power}\n")).unwrap();
    fs::write(frontlight.join("linear_mapping"), "no\n").unwrap();
}

#[test]
fn percent_mapping_rounds_to_native_range() {
    assert_eq!(native_from_percent(0, 2047), 0);
    assert_eq!(native_from_percent(1, 2047), 20);
    assert_eq!(native_from_percent(25, 2047), 512);
    assert_eq!(native_from_percent(100, 2047), 2047);
    assert_eq!(percent_from_native(512, 2047), 25);
}

#[test]
fn detects_frontlight_and_ignores_keyboard_backlight() {
    let root = root("detect");
    let sysfs = root.join("backlight");
    write_frontlight(&sysfs, 512, 2047, 0);
    let keyboard = sysfs.join("rm_keyboard_backlight");
    fs::create_dir_all(&keyboard).unwrap();
    fs::write(keyboard.join("brightness"), "255\n").unwrap();
    fs::write(keyboard.join("max_brightness"), "255\n").unwrap();
    fs::write(keyboard.join("bl_power"), "0\n").unwrap();

    let manager = BacklightManager::load_at(root.join("config.toml"), sysfs);
    let snapshot = manager.snapshot();
    assert!(snapshot.supported);
    assert_eq!(snapshot.provider.as_deref(), Some(FRONTLIGHT_PROVIDER));
    assert_eq!(snapshot.percent, Some(25));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn setting_zero_turns_frontlight_power_off() {
    let root = root("zero");
    let sysfs = root.join("backlight");
    write_frontlight(&sysfs, 512, 2047, 0);
    let manager = BacklightManager::load_at(root.join("config.toml"), sysfs.clone());

    manager.set_percent(0).unwrap();

    assert_eq!(
        fs::read_to_string(sysfs.join(FRONTLIGHT_PROVIDER).join("brightness")).unwrap(),
        "0\n"
    );
    assert_eq!(
        fs::read_to_string(sysfs.join(FRONTLIGHT_PROVIDER).join("bl_power")).unwrap(),
        "4\n"
    );
    assert!(fs::read_to_string(root.join("config.toml"))
        .unwrap()
        .contains("desired_percent = 0"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn force_off_restores_without_overwriting_desired_percent() {
    let root = root("force-restore");
    let sysfs = root.join("backlight");
    write_frontlight(&sysfs, 512, 2047, 0);
    let manager = BacklightManager::load_at(root.join("config.toml"), sysfs.clone());

    manager.set_percent(75).unwrap();
    manager.force_off("test");
    assert_eq!(
        fs::read_to_string(sysfs.join(FRONTLIGHT_PROVIDER).join("brightness")).unwrap(),
        "0\n"
    );
    assert_eq!(
        fs::read_to_string(sysfs.join(FRONTLIGHT_PROVIDER).join("bl_power")).unwrap(),
        "4\n"
    );
    manager.restore_desired();
    assert_eq!(
        fs::read_to_string(sysfs.join(FRONTLIGHT_PROVIDER).join("brightness")).unwrap(),
        "1535\n"
    );
    assert_eq!(
        fs::read_to_string(sysfs.join(FRONTLIGHT_PROVIDER).join("bl_power")).unwrap(),
        "0\n"
    );
    assert!(fs::read_to_string(root.join("config.toml"))
        .unwrap()
        .contains("desired_percent = 75"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unsupported_snapshot_is_reported_without_writing_config() {
    let root = root("unsupported");
    let manager = BacklightManager::load_at(root.join("config.toml"), root.join("missing"));
    let snapshot = manager.snapshot();
    assert!(!snapshot.supported);
    assert!(manager.set_percent(20).is_err());
    assert!(!root.join("config.toml").exists());
    let _ = fs::remove_dir_all(root);
}
