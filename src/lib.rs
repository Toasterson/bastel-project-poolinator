use clap::Parser;
use config::File;
use futures_util::StreamExt;
use lapin::{
    options::{BasicAckOptions, BasicConsumeOptions, BasicPublishOptions, QueueDeclareOptions},
    types::FieldTable,
    BasicProperties,
};
use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use wasi_common::pipe::{ReadPipe, WritePipe};
use wasmtime::*;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder};

#[derive(Debug, Error, Diagnostic)]
pub enum Error {
    #[error(transparent)]
    Config(#[from] config::ConfigError),

    #[error(transparent)]
    PoolError(#[from] deadpool_lapin::CreatePoolError),

    #[error(transparent)]
    LapinError(#[from] deadpool_lapin::lapin::Error),

    #[error(transparent)]
    JsonError(#[from] serde_json::Error),

    #[error(transparent)]
    WasmTime(#[from] wasmtime::Error),

    #[error("{0}")]
    ErrorValue(String),
}

type Result<T> = miette::Result<T, Error>;

#[derive(Parser, Clone)]
pub struct Args {
    pub rabbitmq: Option<String>,

    #[arg(long, short)]
    pub function: Option<String>,
}

const RPC_QUEUE: &str = "pool.rpc";

#[derive(Debug, Serialize, Deserialize)]
enum RPCResponse {
    Success { output: serde_json::Value },
    Error { message: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum RPCRequest {
    Function {
        name: String,
        input: serde_json::Value,
    },
    Pipeline {
        functions: Vec<String>,
        input: serde_json::Value,
    },
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Config {
    rabbitmq: deadpool_lapin::Config,
}

pub fn load_config(args: Args) -> Result<Config> {
    let cfg = config::Config::builder()
        .add_source(File::with_name("/etc/poolinator.yaml").required(false))
        .set_override_option("rabbitmq.url", args.rabbitmq)?
        .build()?;

    Ok(cfg.try_deserialize()?)
}

pub async fn listen(config: Config) -> Result<()> {
    tracing::info!("Starting WASM engine");
    let engine = Engine::default();

    tracing::info!("connecting rmq consumer...");
    let rabbitmq = config
        .rabbitmq
        .create_pool(Some(deadpool::Runtime::Tokio1))?;

    let rmq_con = rabbitmq
        .get()
        .await
        .map_err(|e| Error::ErrorValue(e.to_string()))?;
    let channel = rmq_con.create_channel().await?;

    let queue = channel
        .queue_declare(
            RPC_QUEUE,
            QueueDeclareOptions::default(),
            FieldTable::default(),
        )
        .await?;
    tracing::info!("Declared queue {:?}", queue);

    let mut consumer = channel
        .basic_consume(
            RPC_QUEUE,
            "poolinator.rpc.consumer",
            BasicConsumeOptions::default(),
            FieldTable::default(),
        )
        .await?;

    tracing::info!("rmq consumer connected, waiting for messages");
    while let Some(delivery) = consumer.next().await {
        if let Ok(delivery) = delivery {
            let request: RPCRequest = serde_json::from_slice(&delivery.data)?;
            match handle_message(&engine, request) {
                Ok(reply) => {
                    if let Some(reply_queue) = delivery.properties.reply_to() {
                        let resp = RPCResponse::Success { output: reply };
                        let payload = serde_json::to_vec(&resp)?;
                        channel
                            .basic_publish(
                                "",
                                reply_queue.as_str(),
                                BasicPublishOptions::default(),
                                &payload,
                                BasicProperties::default(),
                            )
                            .await?;
                    }

                    delivery.ack(BasicAckOptions::default()).await?
                }
                Err(err) => {
                    tracing::error!(error = err.to_string(), "failed to handle message");
                    if let Some(reply_queue) = delivery.properties.reply_to() {
                        let err_msg = RPCResponse::Error {
                            message: err.to_string(),
                        };
                        let err_payload = serde_json::to_vec(&err_msg)?;
                        channel
                            .basic_publish(
                                "",
                                reply_queue.as_str(),
                                BasicPublishOptions::default(),
                                &err_payload,
                                BasicProperties::default(),
                            )
                            .await?;
                    }
                    delivery.ack(BasicAckOptions::default()).await?;
                }
            }
        }
    }
    Ok(())
}

fn handle_message(engine: &Engine, request: RPCRequest) -> Result<serde_json::Value> {
    match request {
        RPCRequest::Function { name, input } => {
            let serialized_input = serde_json::to_string(&input)?;

            let mut linker: Linker<WasiCtx> = Linker::new(&engine);
            wasmtime_wasi::add_to_linker(&mut linker, |s| s)?;

            let stdin = ReadPipe::from(serialized_input);
            let stdout = WritePipe::new_in_memory();

            let wasi = WasiCtxBuilder::new()
                .stdin(Box::new(stdin.clone()))
                .stdout(Box::new(stdout.clone()))
                .inherit_stderr()
                .build();
            let mut store = Store::new(&engine, wasi);
            let wasm_path = format!("{}.wasm", name);

            let module = Module::from_file(&engine, &wasm_path)?;
            linker.module(&mut store, "", &module)?;
            linker
                .get_default(&mut store, "")?
                .typed::<(), ()>(&store)?
                .call(&mut store, ())?;

            drop(store);
            drop(module);

            let contents: Vec<u8> = stdout
                .try_into_inner()
                .map_err(|_err| Error::ErrorValue(format!("sole remaining reference")))?
                .into_inner();
            Ok(serde_json::from_slice(&contents)?)
        }
        RPCRequest::Pipeline { .. } => todo!(),
    }
}

pub async fn send_to_rpc_queue(cfg: Config, request: RPCRequest) -> Result<String> {
    tracing::info!("connecting rmq publisher...");
    let payload = serde_json::to_vec(&request)?;
    let rabbitmq = cfg.rabbitmq.create_pool(Some(deadpool::Runtime::Tokio1))?;

    let rmq_con = rabbitmq
        .get()
        .await
        .map_err(|e| Error::ErrorValue(e.to_string()))?;
    let channel = rmq_con.create_channel().await?;
    let callback_queue = channel
        .queue_declare(
            "",
            QueueDeclareOptions {
                exclusive: true,
                auto_delete: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;
    channel
        .basic_publish(
            "",
            RPC_QUEUE,
            BasicPublishOptions::default(),
            payload.as_slice(),
            BasicProperties::default().with_reply_to(callback_queue.name().to_owned()),
        )
        .await?
        .await?;
    let mut consumer = channel
        .basic_consume(
            callback_queue.name().as_str(),
            "poolinator.sender.callback",
            BasicConsumeOptions::default(),
            FieldTable::default(),
        )
        .await?;

    if let Some(delivery) = consumer.next().await {
        let delivery = delivery?;
        let resp_msg: RPCResponse = serde_json::from_slice(&delivery.data)?;
        match resp_msg {
            RPCResponse::Success { output } => Ok(output.to_string()),
            RPCResponse::Error { message } => Err(Error::ErrorValue(message)),
        }
    } else {
        Err(Error::ErrorValue(format!("no response received")))
    }
}
