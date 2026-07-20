use remagic_core::AppId;
use remagic_protocol::{read_frame, write_frame, PackageOperation, Request, Response};
use std::path::PathBuf;
use tokio::net::UnixStream;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
        "packages" => Request::Package {
            operation: parse_package(&args[1..])?,
        },
        _ => return Err(format!("unknown command: {command}").into()),
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
