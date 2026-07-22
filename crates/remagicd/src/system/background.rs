use super::{run, SystemController};
use remagic_core::{AppId, BackgroundRestartPolicy};
use std::path::Path;

impl SystemController {
    pub async fn start_managed_background(
        &self,
        id: &AppId,
        exec: &Path,
        arguments: &[String],
        working_dir: &Path,
        restart: BackgroundRestartPolicy,
        environment: &[(String, String)],
    ) -> Result<String, String> {
        let unit = managed_background_unit(id);
        if self.is_active_checked(&unit).await? {
            return Ok(unit);
        }
        if !exec.is_file() || !working_dir.is_dir() {
            return Err(format!(
                "managed background service for {id} has unavailable paths"
            ));
        }
        let restart = match restart {
            BackgroundRestartPolicy::Never => "no",
            BackgroundRestartPolicy::OnFailure => "on-failure",
            BackgroundRestartPolicy::Always => "always",
        };
        let mut systemd_args = systemd_run_arguments(&unit, working_dir, restart, environment)?;
        systemd_args.push("--".into());
        systemd_args.push(exec.display().to_string());
        systemd_args.extend(arguments.iter().cloned());
        let refs = systemd_args.iter().map(String::as_str).collect::<Vec<_>>();
        run("systemd-run", &refs).await?;
        self.wait_active(&unit).await?;
        Ok(unit)
    }
}

pub fn managed_background_unit(id: &AppId) -> String {
    format!("remagic-background-{}.service", id.as_str())
}

fn systemd_run_arguments(
    unit: &str,
    working_dir: &Path,
    restart: &str,
    environment: &[(String, String)],
) -> Result<Vec<String>, String> {
    let mut arguments = vec![
        format!("--unit={unit}"),
        "--collect".into(),
        "--service-type=simple".into(),
        format!("--property=Restart={restart}"),
        "--property=RestartSec=2s".into(),
        "--property=TimeoutStopSec=5s".into(),
        format!("--working-directory={}", working_dir.display()),
    ];
    for (key, value) in environment {
        if key.contains('=') || key.as_bytes().contains(&0) || value.as_bytes().contains(&0) {
            return Err(format!(
                "invalid managed background environment key {key:?}"
            ));
        }
        arguments.push(format!("--setenv={key}={value}"));
    }
    Ok(arguments)
}
