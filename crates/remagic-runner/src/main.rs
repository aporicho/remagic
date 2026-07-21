mod bootstrap;
mod data_schema;
mod event_loop;
mod executor;
mod lifecycle_bridge;
mod process;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let prepared = bootstrap::prepare()?;
    let running = process::launch(prepared).await?;
    event_loop::supervise(running).await
}
