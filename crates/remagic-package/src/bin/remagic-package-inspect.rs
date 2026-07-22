use remagic_core::{DeviceProduct, DeviceProfile};
use remagic_package::PackageManager;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.len() != 4 || !matches!(arguments[0].as_str(), "verify" | "install") {
        return Err("usage: remagic-package-inspect <verify|install> BUNDLE DEVICE OS".into());
    }
    let product = match arguments[2].as_str() {
        "paper_pro" | "ferrari" => DeviceProduct::PaperPro,
        "paper_pro_move" | "chiappa" => DeviceProduct::PaperProMove,
        other => return Err(format!("unsupported device: {other}").into()),
    };
    let profile = DeviceProfile::for_product(product, &arguments[3]);
    let manager = PackageManager::from_environment();
    let prepared = manager.prepare(Path::new(&arguments[1]), &profile)?;
    if arguments[0] == "install" {
        let outcome = manager.install(prepared)?;
        println!(
            "installed {} {} {}",
            outcome.app_id, outcome.version, outcome.content_id
        );
    } else {
        println!(
            "verified {} {} {}",
            prepared.bundle.app_id, prepared.bundle.version, prepared.bundle.content_id
        );
    }
    Ok(())
}
