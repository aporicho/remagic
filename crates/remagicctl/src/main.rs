use remagic_core::AppId;
use remagic_protocol::{read_frame, write_frame, PackageOperation, Request, Response};
use std::path::PathBuf;
use tokio::net::UnixStream;

mod display;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("display-status")) {
        return display::json_command(serde_json::json!({"command": "status"})).await;
    }
    if matches!(
        args.first().map(String::as_str),
        Some("display-submissions")
    ) {
        return display::submissions().await;
    }
    if matches!(args.first().map(String::as_str), Some("display-signature")) {
        let key = parse_coordinate(args.get(1), "display-signature key")?;
        return display::surface_signature(key).await;
    }
    if matches!(args.first().map(String::as_str), Some("refresh")) {
        return display::json_command(serde_json::json!({"command": "request_full_refresh"})).await;
    }
    if matches!(args.first().map(String::as_str), Some("tap")) {
        let x = parse_coordinate(args.get(1), "tap X")?;
        let y = parse_coordinate(args.get(2), "tap Y")?;
        return display::json_command(serde_json::json!({
            "command": "inject_tap",
            "x": x,
            "y": y,
        }))
        .await;
    }
    if matches!(args.first().map(String::as_str), Some("pen-line")) {
        let x0 = parse_coordinate(args.get(1), "pen-line X0")?;
        let y0 = parse_coordinate(args.get(2), "pen-line Y0")?;
        let x1 = parse_coordinate(args.get(3), "pen-line X1")?;
        let y1 = parse_coordinate(args.get(4), "pen-line Y1")?;
        return display::json_command(serde_json::json!({
            "command": "inject_pen_line",
            "x0": x0,
            "y0": y0,
            "x1": x1,
            "y1": y1,
            "points": 24,
        }))
        .await;
    }
    let request = parse(&args)?;
    let socket =
        std::env::var("REMAGIC_SOCKET").unwrap_or_else(|_| remagic_protocol::DEFAULT_SOCKET.into());
    let mut stream = UnixStream::connect(socket).await?;
    write_frame(&mut stream, &request).await?;
    let response: Response = read_frame(&mut stream).await?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    if matches!(response, Response::Error { .. }) {
        std::process::exit(1);
    }
    Ok(())
}

fn parse_coordinate(
    value: Option<&String>,
    label: &str,
) -> Result<i32, Box<dyn std::error::Error>> {
    value
        .ok_or_else(|| format!("{label} requires a value").into())
        .and_then(|value| {
            value
                .parse::<i32>()
                .map_err(|_| format!("{label} must be an integer").into())
        })
}

fn parse(args: &[String]) -> Result<Request, Box<dyn std::error::Error>> {
    let command = args.first().map(String::as_str).unwrap_or("status");
    Ok(match command {
        "status" => Request::Status,
        "apps" => Request::ListApps,
        "reload" => Request::ReloadManifests,
        "manager" => Request::OpenManager,
        "system" => Request::ReturnSystem,
        "sleep" => Request::Sleep,
        "park" => Request::ParkCurrent,
        "launch" => {
            let id = AppId::new(args.get(1).ok_or("launch requires an app id")?.clone())?;
            let open_path = args
                .windows(2)
                .find(|window| window[0] == "--open-path")
                .map(|window| PathBuf::from(&window[1]));
            Request::Launch {
                app_id: id,
                open_path,
            }
        }
        "close" => Request::Close {
            app_id: AppId::new(args.get(1).ok_or("close requires an app id")?.clone())?,
            complete: args.iter().any(|arg| arg == "--complete"),
        },
        "runtime-exited" => parse_runtime_exited(&args[1..])?,
        "packages" => Request::Package {
            operation: parse_package(&args[1..])?,
        },
        _ => return Err(format!("unknown command: {command}").into()),
    })
}

fn parse_runtime_exited(args: &[String]) -> Result<Request, Box<dyn std::error::Error>> {
    let app_id = AppId::new(
        args.first()
            .ok_or("runtime-exited requires an app id")?
            .clone(),
    )?;
    let mut generation = None;
    let mut exit_code = None;
    let mut crashed = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--generation" => {
                if generation.is_some() {
                    return Err("runtime-exited generation was specified more than once".into());
                }
                let value = args
                    .get(index + 1)
                    .ok_or("runtime-exited --generation requires a value")?;
                let parsed = value
                    .parse::<u64>()
                    .map_err(|_| "runtime-exited generation must be an unsigned integer")?;
                if parsed == 0 {
                    return Err("runtime-exited generation must be non-zero".into());
                }
                generation = Some(parsed);
                index += 2;
            }
            "--exit-code" => {
                if exit_code.is_some() {
                    return Err("runtime-exited exit code was specified more than once".into());
                }
                let value = args
                    .get(index + 1)
                    .ok_or("runtime-exited --exit-code requires a value")?;
                exit_code = Some(
                    value
                        .parse::<i32>()
                        .map_err(|_| "runtime-exited exit code must be a signed integer")?,
                );
                index += 2;
            }
            "--crashed" => {
                if crashed {
                    return Err("runtime-exited --crashed was specified more than once".into());
                }
                crashed = true;
                index += 1;
            }
            option => return Err(format!("unknown runtime-exited option: {option}").into()),
        }
    }
    Ok(Request::RuntimeExited {
        app_id,
        generation: generation.ok_or("runtime-exited requires --generation")?,
        exit_code: exit_code.ok_or("runtime-exited requires --exit-code")?,
        crashed,
    })
}

fn parse_package(args: &[String]) -> Result<PackageOperation, Box<dyn std::error::Error>> {
    Ok(
        match args.first().map(String::as_str).unwrap_or("refresh") {
            "bootstrap" => PackageOperation::Bootstrap,
            "refresh" => PackageOperation::Refresh,
            "search" => PackageOperation::Search {
                query: args.get(1).cloned().unwrap_or_default(),
            },
            "info" => PackageOperation::Info {
                package: args.get(1).ok_or("info requires a package")?.clone(),
            },
            "install" => PackageOperation::Install {
                package: args.get(1).ok_or("install requires a package")?.clone(),
            },
            "remove" => PackageOperation::Remove {
                package: args.get(1).ok_or("remove requires a package")?.clone(),
                purge: args.iter().any(|arg| arg == "--purge"),
            },
            "upgrade" => PackageOperation::Upgrade,
            other => return Err(format!("unknown package operation: {other}").into()),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parses_generation_tokenized_runtime_exit() {
        let request = parse(&args(&[
            "runtime-exited",
            "magicpaper",
            "--generation",
            "913",
            "--exit-code",
            "0",
        ]))
        .unwrap();
        assert!(matches!(
            request,
            Request::RuntimeExited {
                app_id,
                generation: 913,
                exit_code: 0,
                crashed: false,
            } if app_id.as_str() == "magicpaper"
        ));
    }

    #[test]
    fn rejects_runtime_exit_without_generation() {
        let error = parse(&args(&[
            "runtime-exited",
            "koreader",
            "--exit-code",
            "1",
            "--crashed",
        ]))
        .unwrap_err();
        assert!(error.to_string().contains("requires --generation"));
    }

    #[test]
    fn rejects_zero_runtime_generation() {
        let error = parse(&args(&[
            "runtime-exited",
            "koreader",
            "--generation",
            "0",
            "--exit-code",
            "0",
        ]))
        .unwrap_err();
        assert!(error.to_string().contains("must be non-zero"));
    }
}
