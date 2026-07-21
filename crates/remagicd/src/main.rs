mod app_runtime;
mod daemon;
mod display_host;
mod power_device;
mod system;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "remagicd=info".into()),
        )
        .init();
    daemon::run().await
}
