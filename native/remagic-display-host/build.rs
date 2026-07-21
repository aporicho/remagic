fn main() {
    println!("cargo:rerun-if-env-changed=QUILL_LIB_DIR");
    if std::env::var_os("CARGO_FEATURE_DEVICE").is_some() {
        if let Some(path) = std::env::var_os("QUILL_LIB_DIR") {
            println!("cargo:rustc-link-search=native={}", path.to_string_lossy());
        }
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib");
    }
}
