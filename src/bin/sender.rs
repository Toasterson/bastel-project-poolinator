use clap::Parser;
use miette::IntoDiagnostic;
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
    let cfg = load_config(args.clone())?;
    let val: serde_json::Value = serde_json::from_reader(std::io::stdin()).into_diagnostic()?;
    let req = RPCRequest::Function {
        name: args.function.unwrap(),
        input: val,
    };
    let resp = send_to_rpc_queue(cfg, req).await?;
    println!("{resp}");
    Ok(())
}
