#[cfg(feature = "device")]
use remagic_core::DomainState;
use remagic_protocol::{read_frame, write_frame, AppView, Request, Response};
use tokio::net::UnixStream;

#[cfg(feature = "device")]
mod device;
#[cfg(feature = "device")]
mod qtfb;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let apps = list_apps().await?;
    #[cfg(feature = "device")]
    return device::run(apps).await;
    #[cfg(not(feature = "device"))]
    {
        println!("ReMagic");
        for app in apps {
            println!("- {} ({})", app.name, app.id);
        }
        Ok(())
    }
}

async fn list_apps() -> Result<Vec<AppView>, Box<dyn std::error::Error>> {
    let mut stream = UnixStream::connect(remagic_protocol::DEFAULT_SOCKET).await?;
    write_frame(&mut stream, &Request::ListApps).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Apps { apps } => Ok(apps),
        Response::Error { message } => Err(message.into()),
        _ => Err("unexpected manager response".into()),
    }
}

#[cfg(feature = "device")]
async fn domain_state() -> Result<DomainState, Box<dyn std::error::Error>> {
    let mut stream = UnixStream::connect(remagic_protocol::DEFAULT_SOCKET).await?;
    write_frame(&mut stream, &Request::Status).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Status { domain, .. } => Ok(domain),
        Response::Error { message } => Err(message.into()),
        _ => Err("unexpected manager response".into()),
    }
}

#[cfg(feature = "device")]
async fn request(request: Request) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = UnixStream::connect(remagic_protocol::DEFAULT_SOCKET).await?;
    write_frame(&mut stream, &request).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Ok => Ok(()),
        Response::Error { message } => Err(message.into()),
        _ => Err("unexpected manager response".into()),
    }
}

#[cfg(feature = "device")]
async fn power_status() -> Result<remagic_core::PowerSnapshot, Box<dyn std::error::Error>> {
    power_request(Request::PowerStatus).await
}

#[cfg(feature = "device")]
async fn set_idle_suspend(
    seconds: u64,
) -> Result<remagic_core::PowerSnapshot, Box<dyn std::error::Error>> {
    power_request(Request::SetIdleSuspend { seconds }).await
}

#[cfg(feature = "device")]
async fn power_request(
    request: Request,
) -> Result<remagic_core::PowerSnapshot, Box<dyn std::error::Error>> {
    let mut stream = UnixStream::connect(remagic_protocol::DEFAULT_SOCKET).await?;
    write_frame(&mut stream, &request).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Power { snapshot } => Ok(snapshot),
        Response::Error { message } => Err(message.into()),
        _ => Err("unexpected manager power response".into()),
    }
}
