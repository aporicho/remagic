fn main() {
    if std::env::var_os("CARGO_FEATURE_DEVICE").is_none() {
        return;
    }
    let quill = std::env::var("QUILL_DIR")
        .unwrap_or_else(|_| format!("{}/../../../quill-move", env!("CARGO_MANIFEST_DIR")));
    println!("cargo:rerun-if-env-changed=QUILL_DIR");
    println!("cargo:rustc-link-search=native={quill}/build");
    println!("cargo:rustc-link-search=native={quill}/vendor");
    println!("cargo:rustc-link-lib=dylib=quill");
    println!("cargo:rustc-link-lib=dylib=qsgepaper");
    println!(
        "cargo:rustc-link-arg=-Wl,-rpath,/home/root/apps/remagic/lib:/usr/lib/plugins/scenegraph"
    );
    if let Ok(sysroot) = std::env::var("SDKTARGETSYSROOT") {
        println!("cargo:rustc-link-arg=-Wl,-rpath-link,{sysroot}/usr/lib");
    }
}
