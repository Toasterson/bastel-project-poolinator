use clap::Parser;
use poolinator::*;
use tracing_subscriber::prelude::*;

#[tokio::main]
async fn main() -> miette::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "poolinator=trace".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
    let args = Args::parse();
    let cfg = load_config(args)?;
    listen(cfg).await?;
    Ok(())
}
