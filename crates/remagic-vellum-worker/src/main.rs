use remagic_core::{DeviceProduct, DeviceProfile, REMAGIC_APP_API_VERSION};
use remagic_protocol::PackageOperation;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;

const LOG_DIR: &str = "/home/root/.local/state/remagic/packages";
const STORE_BINARY: &str = "/home/root/apps/remagic-store/current/payload/bin/remagic-store";

#[derive(Clone, Debug, Eq, PartialEq)]
struct StoreContext {
    product: &'static str,
    os_version: String,
    api: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let encoded = std::env::args().nth(1).ok_or("missing package operation")?;
    let operation: PackageOperation = serde_json::from_str(&encoded)?;
    validate_operation(&operation)?;
    let started = unix_now();
    let result = execute(&operation).await;
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
            return Err("this package is excluded by ReMagic policy".into());
        }
    }
    if let PackageOperation::Search { query } = operation {
        if query.len() > 128 || query.as_bytes().contains(&0) {
            return Err("unsafe package search".into());
        }
    }
    Ok(())
}

async fn execute(operation: &PackageOperation) -> Result<String, String> {
    let store = Path::new(STORE_BINARY);
    if !store.is_file() {
        return Err("ReMagic Store is not installed; update ReMagic to restore it".into());
    }
    if matches!(operation, PackageOperation::Bootstrap) {
        return Ok("ReMagic Store is installed".into());
    }
    let context = store_context()?;
    match operation {
        PackageOperation::Bootstrap => unreachable!(),
        PackageOperation::Refresh => catalog(store, &context).await,
        PackageOperation::Search { query } => {
            let output = catalog(store, &context).await?;
            filter_catalog(&output, Some(query), None)
        }
        PackageOperation::Info { package } => {
            let output = catalog(store, &context).await?;
            filter_catalog(&output, None, Some(package))
        }
        PackageOperation::Install { package } => {
            run_store(store, install_arguments("install", package, &context)).await
        }
        PackageOperation::Remove { package, purge } => {
            let mut arguments = vec!["uninstall".into(), package.clone()];
            if *purge {
                arguments.push("--purge".into());
            }
            run_store(store, arguments).await
        }
        PackageOperation::Upgrade => upgrade_all(store, &context).await,
    }
}

fn store_context() -> Result<StoreContext, String> {
    let device = DeviceProfile::detect().map_err(|error| error.to_string())?;
    Ok(StoreContext {
        product: match device.product {
            DeviceProduct::PaperPro => "paper_pro",
            DeviceProduct::PaperProMove => "paper_pro_move",
        },
        os_version: device.os_version,
        api: REMAGIC_APP_API_VERSION.to_string(),
    })
}

async fn catalog(store: &Path, context: &StoreContext) -> Result<String, String> {
    run_store(
        store,
        vec![
            "catalog".into(),
            context.product.into(),
            context.os_version.clone(),
            context.api.clone(),
        ],
    )
    .await
}

fn install_arguments(operation: &str, app_id: &str, context: &StoreContext) -> Vec<String> {
    vec![
        operation.into(),
        app_id.into(),
        context.product.into(),
        context.os_version.clone(),
        context.api.clone(),
    ]
}

async fn upgrade_all(store: &Path, context: &StoreContext) -> Result<String, String> {
    let output = catalog(store, context).await?;
    let updates = update_app_ids(&output)?;
    if updates.is_empty() {
        return Ok("all applications are current".into());
    }
    let mut completed = Vec::new();
    for app_id in updates {
        let output = run_store(store, install_arguments("upgrade", &app_id, context)).await?;
        completed.push(format!("{app_id}: {}", output.trim()));
    }
    Ok(completed.join("\n"))
}

fn filter_catalog(
    output: &str,
    query: Option<&str>,
    app_id: Option<&str>,
) -> Result<String, String> {
    let mut value: Value = serde_json::from_str(output)
        .map_err(|error| format!("ReMagic Store returned invalid catalog JSON: {error}"))?;
    let apps = value
        .get_mut("apps")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "ReMagic Store catalog has no apps array".to_owned())?;
    let query = query.map(str::to_lowercase);
    apps.retain(|app| {
        if let Some(expected) = app_id {
            return app.get("id").and_then(Value::as_str) == Some(expected);
        }
        let Some(query) = &query else { return true };
        ["id", "name", "summary"].iter().any(|field| {
            app.get(field)
                .and_then(Value::as_str)
                .is_some_and(|value| value.to_lowercase().contains(query))
        })
    });
    if app_id.is_some() && apps.is_empty() {
        return Err(format!(
            "application is not in the signed catalog: {}",
            app_id.unwrap_or_default()
        ));
    }
    serde_json::to_string_pretty(&value).map_err(|error| error.to_string())
}

fn update_app_ids(output: &str) -> Result<Vec<String>, String> {
    let value: Value = serde_json::from_str(output)
        .map_err(|error| format!("ReMagic Store returned invalid catalog JSON: {error}"))?;
    let apps = value
        .get("apps")
        .and_then(Value::as_array)
        .ok_or_else(|| "ReMagic Store catalog has no apps array".to_owned())?;
    Ok(apps
        .iter()
        .filter(|app| {
            app.get("status")
                .and_then(|status| status.get("status"))
                .and_then(Value::as_str)
                == Some("update_available")
        })
        .filter_map(|app| app.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect())
}

async fn run_store(store: &Path, arguments: Vec<String>) -> Result<String, String> {
    let output = Command::new(store)
        .args(arguments)
        .env_clear()
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("HOME", "/home/root")
        .env("REMAGIC_API_VERSION", REMAGIC_APP_API_VERSION.to_string())
        .env("REMAGIC_CONTROL_SOCKET", remagic_protocol::DEFAULT_SOCKET)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| error.to_string())?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if output.status.success() {
        Ok(text)
    } else if text.trim().is_empty() {
        Err(format!("ReMagic Store exited with {}", output.status))
    } else {
        Err(text)
    }
}

fn write_status(value: &Value) -> Result<(), Box<dyn std::error::Error>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    const CATALOG: &str = r#"{
      "apps": [
        {"id":"magicpaper","name":"MagicPaper","summary":"手写 AI", "status":{"status":"installed"}},
        {"id":"koreader","name":"KOReader","summary":"电子书", "status":{"status":"update_available"}}
      ]
    }"#;

    #[test]
    fn signed_catalog_views_are_filtered_without_exposing_another_app() {
        let search: Value =
            serde_json::from_str(&filter_catalog(CATALOG, Some("电子"), None).unwrap()).unwrap();
        assert_eq!(search["apps"].as_array().unwrap().len(), 1);
        assert_eq!(search["apps"][0]["id"], "koreader");

        let info: Value =
            serde_json::from_str(&filter_catalog(CATALOG, None, Some("magicpaper")).unwrap())
                .unwrap();
        assert_eq!(info["apps"].as_array().unwrap().len(), 1);
        assert!(filter_catalog(CATALOG, None, Some("missing")).is_err());
    }

    #[test]
    fn upgrade_all_selects_only_catalog_updates() {
        assert_eq!(update_app_ids(CATALOG).unwrap(), vec!["koreader"]);
    }

    #[test]
    fn store_install_arguments_carry_the_detected_device_contract() {
        let context = StoreContext {
            product: "paper_pro_move",
            os_version: "3.27.3.0".into(),
            api: "6".into(),
        };
        assert_eq!(
            install_arguments("upgrade", "koreader", &context),
            ["upgrade", "koreader", "paper_pro_move", "3.27.3.0", "6"]
        );
    }
}
