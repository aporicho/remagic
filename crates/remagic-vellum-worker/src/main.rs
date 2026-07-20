use remagic_protocol::PackageOperation;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;

const LOG_DIR: &str = "/home/root/.local/state/remagic/packages";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let encoded = std::env::args().nth(1).ok_or("missing package operation")?;
    let operation: PackageOperation = serde_json::from_str(&encoded)?;
    validate_operation(&operation)?;
    let started = unix_now();
    let result = match &operation {
        PackageOperation::Bootstrap => bootstrap().await,
        _ => match find_vellum() {
            Some(vellum) => execute(&vellum, &operation).await,
            None => Err("Vellum is not installed".into()),
        },
    };
    let (success, output) = match result {
        Ok(output) => (true, output),
        Err(output) => (false, output),
    };
    write_status(&json!({
        "operation": operation,
        "started_at": started,
        "finished_at": unix_now(),
        "success": success,
        "output": output,
    }))?;
    println!("{output}");
    if success {
        let _ = Command::new("/home/root/apps/remagic/bin/remagicctl")
            .arg("reload")
            .status()
            .await;
        Ok(())
    } else {
        std::process::exit(1);
    }
}

fn find_vellum() -> Option<PathBuf> {
    ["/home/root/.vellum/bin/vellum", "/usr/bin/vellum"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

fn validate_operation(operation: &PackageOperation) -> Result<(), Box<dyn std::error::Error>> {
    let package = match operation {
        PackageOperation::Info { package }
        | PackageOperation::Install { package }
        | PackageOperation::Remove { package, .. } => Some(package),
        _ => None,
    };
    if let Some(package) = package {
        let normalized = package.to_ascii_lowercase();
        let safe = !package.is_empty()
            && package.len() <= 128
            && package.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@')
            });
        if !safe {
            return Err("unsafe package name".into());
        }
        if normalized.contains("oxide") {
            return Err("this package is excluded by Remagic policy".into());
        }
    }
    Ok(())
}

async fn execute(vellum: &Path, operation: &PackageOperation) -> Result<String, String> {
    let arguments: Vec<String> = match operation {
        PackageOperation::Bootstrap => return Err("Vellum is already installed".into()),
        PackageOperation::Refresh => vec!["update".into()],
        PackageOperation::Search { query } => vec!["search".into(), query.clone()],
        PackageOperation::Info { package } => vec!["info".into(), package.clone()],
        PackageOperation::Install { package } => {
            let info = run(vellum, &["info".into(), package.clone()]).await?;
            if info.to_ascii_lowercase().contains("oxide") {
                return Err("dependency preflight found an excluded package".into());
            }
            vec!["add".into(), package.clone()]
        }
        PackageOperation::Remove { package, purge } => {
            vec![if *purge { "purge" } else { "del" }.into(), package.clone()]
        }
        PackageOperation::Upgrade => vec!["upgrade".into()],
    };
    run(vellum, &arguments).await
}

async fn bootstrap() -> Result<String, String> {
    if find_vellum().is_some() {
        return Ok("Vellum is already installed".into());
    }
    let script = Path::new("/home/root/apps/remagic/share/vellum-bootstrap.sh");
    if !script.is_file() {
        return Err("verified Vellum bootstrap is missing from Remagic".into());
    }
    run(Path::new("/bin/sh"), &[script.display().to_string()]).await
}

async fn run(program: &Path, arguments: &[String]) -> Result<String, String> {
    let output = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if output.status.success() {
        Ok(text)
    } else {
        Err(text)
    }
}

fn write_status(value: &serde_json::Value) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(LOG_DIR)?;
    let path = Path::new(LOG_DIR).join("last.json");
    let temp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(&temp, bytes)?;
    fs::rename(temp, path)?;
    Ok(())
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}
