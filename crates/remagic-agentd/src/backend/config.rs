use remagic_core::AppId;
use remagic_protocol::AgentProfile;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

pub(super) const SAFE_TOOLS_EXTENSION: &str =
    "/home/root/apps/remagic/runtime/pi/extensions/remagic-tools.js";
const MAX_PROVIDER_FILE_BYTES: usize = 64 * 1024;

pub(super) fn configured_command(
    pi_binary: &Path,
    app_id: &AppId,
    profile: &AgentProfile,
    system_prompt: &str,
) -> Result<Command, String> {
    let data_root = app_data_root(app_id);
    let home = data_root.join("home");
    let config_root = data_root.join("config");
    std::fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    let mut command = Command::new(pi_binary);
    command
        .current_dir(&data_root)
        .env_clear()
        .env("HOME", &home)
        .env("PI_CODING_AGENT_DIR", &config_root)
        .env("PI_SKIP_VERSION_CHECK", "1")
        .env("PI_TELEMETRY", "0")
        .env(
            "PATH",
            "/home/root/apps/remagic/runtime/pi/bin:/usr/bin:/bin",
        )
        .env("LANG", "C.UTF-8")
        .args(["--mode", "rpc", "--provider"])
        .arg(&profile.provider)
        .arg("--model")
        .arg(&profile.model)
        .arg("--thinking")
        .arg(&profile.thinking)
        .args([
            "--no-session",
            "--no-skills",
            "--no-prompt-templates",
            "--no-context-files",
            "--no-approve",
            "--system-prompt",
        ])
        .arg(system_prompt)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    configure_tools(&mut command, profile, Path::new(SAFE_TOOLS_EXTENSION))?;
    load_provider_secret(&mut command, &config_root, &profile.provider)?;
    Ok(command)
}

fn configure_tools(
    command: &mut Command,
    profile: &AgentProfile,
    safe_extension: &Path,
) -> Result<(), String> {
    if !profile.tools {
        command.args(["--no-tools", "--no-extensions"]);
        return Ok(());
    }
    if !secure_regular_file(safe_extension) {
        return Err("ReMagic safe Pi tools extension is unavailable or unsafe".into());
    }
    command
        .args(["--no-builtin-tools", "--no-extensions", "--extension"])
        .arg(safe_extension);
    Ok(())
}

#[cfg(not(test))]
fn app_data_root(app_id: &AppId) -> PathBuf {
    Path::new("/home/root/.local/share/remagic/agent/apps").join(app_id.as_str())
}

#[cfg(test)]
fn app_data_root(app_id: &AppId) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(1);
    std::env::temp_dir()
        .join(format!(
            "remagic-agentd-data-{}-{}",
            std::process::id(),
            NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed),
        ))
        .join(app_id.as_str())
}

fn load_provider_secret(
    command: &mut Command,
    config_root: &Path,
    provider: &str,
) -> Result<(), String> {
    let secret = provider_secret(provider)?;
    write_models_config(config_root, provider, secret.as_ref())?;
    if let Some(secret) = secret {
        command.env(secret.key_name, secret.value);
    }
    Ok(())
}

pub(crate) fn provider_configured(provider: &str) -> bool {
    provider_secret(provider).is_ok_and(|secret| secret.is_some())
}

struct ProviderSecret {
    key_name: &'static str,
    value: String,
    base_url: Option<String>,
}

fn provider_secret(provider: &str) -> Result<Option<ProviderSecret>, String> {
    let (key, filename) = match provider {
        "deepseek" => ("DEEPSEEK_API_KEY", "deepseek.env"),
        "openai" | "openai-codex" => ("OPENAI_API_KEY", "openai.env"),
        _ => return Ok(None),
    };
    let path = Path::new("/home/root/.config/remagic/secrets/providers").join(filename);
    if !path.exists() {
        return Ok(None);
    }
    if !private_secret_file(&path) {
        return Err(format!(
            "provider configuration is unsafe: {}",
            path.display()
        ));
    }
    provider_secret_from(&path, key)
}

fn provider_secret_from(path: &Path, key: &'static str) -> Result<Option<ProviderSecret>, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut text = String::with_capacity(1024);
    file.take((MAX_PROVIDER_FILE_BYTES + 1) as u64)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    if text.len() > MAX_PROVIDER_FILE_BYTES {
        return Err("provider configuration exceeds 64 KiB".into());
    }
    let mut secret = None;
    let mut base_url = None;
    for line in text.lines() {
        let Some((candidate, value)) = line.trim().split_once('=') else {
            continue;
        };
        let value = value.trim_matches(['"', '\'']);
        if candidate == key && valid_secret_value(value) {
            secret = Some(value.to_owned());
        } else if candidate == "BASE_URL" {
            if !valid_base_url(value) {
                return Err("provider BASE_URL must be a valid http(s) URL".into());
            }
            base_url = Some(value.to_owned());
        }
    }
    Ok(secret.map(|value| ProviderSecret {
        key_name: key,
        value,
        base_url,
    }))
}

fn valid_secret_value(value: &str) -> bool {
    !value.is_empty() && value.len() <= 16 * 1024 && !value.chars().any(char::is_control)
}

fn valid_base_url(value: &str) -> bool {
    if value.len() > 2048
        || value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return false;
    }
    let authority = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .and_then(|rest| rest.split('/').next());
    authority.is_some_and(|value| !value.is_empty())
}

fn write_models_config(
    config_root: &Path,
    provider: &str,
    secret: Option<&ProviderSecret>,
) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(config_root)
        .map_err(|error| error.to_string())?;
    std::fs::set_permissions(config_root, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    let destination = config_root.join("models.json");
    let Some(secret) = secret.filter(|secret| secret.base_url.is_some()) else {
        remove_owned_config(&destination)?;
        return Ok(());
    };
    let document = serde_json::json!({
        "providers": {
            (provider): {
                "baseUrl": secret.base_url.as_deref().expect("filtered above"),
                "apiKey": format!("${}", secret.key_name),
            }
        }
    });
    let temporary = config_root.join(format!(".models.json.tmp-{}", std::process::id()));
    remove_owned_config(&temporary)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut file, &document).map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, &destination).map_err(|error| error.to_string())?;
    Ok(())
}

fn remove_owned_config(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(format!(
            "refusing unsafe Pi configuration path: {}",
            path.display()
        )),
        Ok(_) => std::fs::remove_file(path).map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn secure_regular_file(path: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    std::fs::symlink_metadata(path).is_ok_and(|metadata| {
        !metadata.file_type().is_symlink()
            && metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o022 == 0
    })
}

fn private_secret_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    secure_regular_file(path)
        && std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o077 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    fn extension() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "remagic-agentd-extension-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::write(&path, "export default function () {}\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        path
    }

    fn profile(tools: bool) -> AgentProfile {
        AgentProfile {
            provider: "deepseek".into(),
            model: "deepseek-chat".into(),
            thinking: "off".into(),
            tools,
        }
    }

    #[test]
    fn tools_off_disables_every_pi_tool_source() {
        let extension = extension();
        let mut command = Command::new("pi");
        configure_tools(&mut command, &profile(false), &extension).unwrap();
        let args = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args, ["--no-tools", "--no-extensions"]);
        std::fs::remove_file(extension).unwrap();
    }

    #[test]
    fn tools_on_loads_only_the_fixed_safe_extension() {
        let extension = extension();
        let mut command = Command::new("pi");
        configure_tools(&mut command, &profile(true), &extension).unwrap();
        let args = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "--no-builtin-tools",
                "--no-extensions",
                "--extension",
                extension.to_str().unwrap(),
            ]
        );
        std::fs::remove_file(extension).unwrap();
    }

    #[test]
    fn tools_on_fails_closed_without_a_safe_extension() {
        let mut command = Command::new("pi");
        assert!(configure_tools(
            &mut command,
            &profile(true),
            Path::new("/definitely/missing/remagic-tools.js"),
        )
        .is_err());
    }

    #[test]
    fn provider_secret_permissions_are_private() {
        let secret = extension();
        assert!(private_secret_file(&secret));
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(!private_secret_file(&secret));
        std::fs::remove_file(secret).unwrap();
    }

    #[test]
    fn controlled_base_url_never_writes_the_api_key_to_models_json() {
        let secret_path = extension();
        std::fs::write(
            &secret_path,
            "DEEPSEEK_API_KEY=secret-value\nBASE_URL=https://proxy.example/v1\n",
        )
        .unwrap();
        let secret = provider_secret_from(&secret_path, "DEEPSEEK_API_KEY")
            .unwrap()
            .unwrap();
        let config_marker = extension();
        std::fs::remove_file(&config_marker).unwrap();
        let config = config_marker.with_extension("config");
        write_models_config(&config, "deepseek", Some(&secret)).unwrap();
        let models = std::fs::read_to_string(config.join("models.json")).unwrap();
        assert!(models.contains("https://proxy.example/v1"));
        assert!(models.contains("$DEEPSEEK_API_KEY"));
        assert!(!models.contains("secret-value"));
        std::fs::remove_dir_all(config).unwrap();
        std::fs::remove_file(secret_path).unwrap();
    }

    #[test]
    fn unsafe_provider_base_url_is_rejected() {
        let secret_path = extension();
        std::fs::write(
            &secret_path,
            "DEEPSEEK_API_KEY=secret-value\nBASE_URL=file:///etc/passwd\n",
        )
        .unwrap();
        assert!(provider_secret_from(&secret_path, "DEEPSEEK_API_KEY").is_err());
        std::fs::remove_file(secret_path).unwrap();
    }

    #[test]
    fn oversized_provider_configuration_is_rejected_before_parsing() {
        let secret_path = extension();
        std::fs::write(&secret_path, vec![b'x'; MAX_PROVIDER_FILE_BYTES + 1]).unwrap();
        let Err(error) = provider_secret_from(&secret_path, "DEEPSEEK_API_KEY") else {
            panic!("oversized provider configuration was accepted");
        };
        assert!(error.contains("64 KiB"));
        std::fs::remove_file(secret_path).unwrap();
    }
}
