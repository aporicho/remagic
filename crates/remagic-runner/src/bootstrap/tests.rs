use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FILE: AtomicU64 = AtomicU64::new(1);

fn temporary_file(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "remagic-runner-{label}-{}-{}",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ))
}

#[test]
fn launch_descriptor_preserves_generation_and_qtfb_key() {
    let path = temporary_file("launch");
    fs::write(
        &path,
        br#"{"generation":99,"foreground_epoch":7,"lease_id":88,"qtfb_key":1234,"open_path":"/home/root/books/a.epub"}"#,
    )
    .unwrap();
    let descriptor = read_launch_descriptor(&path, MANIFEST_SCHEMA_V2).unwrap();
    assert_eq!(descriptor.generation, Some(99));
    assert_eq!(descriptor.foreground_epoch, Some(7));
    assert_eq!(descriptor.lease_id, Some(88));
    assert_eq!(descriptor.qtfb_key, Some(1234));
    assert_eq!(
        descriptor.open_path,
        Some(PathBuf::from("/home/root/books/a.epub"))
    );
    fs::remove_file(path).unwrap();
}

#[test]
fn invalid_descriptor_is_rejected_by_v2_but_ignored_by_v1() {
    let path = temporary_file("invalid-launch");
    fs::write(&path, b"not-json").unwrap();
    assert_eq!(
        read_launch_descriptor(&path, 1).unwrap(),
        LaunchDescriptor::default()
    );
    assert_eq!(
        read_launch_descriptor(&path, MANIFEST_SCHEMA_V2)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    fs::remove_file(path).unwrap();
}
