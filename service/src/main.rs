#![allow(clippy::large_enum_variant)]
#![allow(dead_code)]

use tonic::transport::Server;
mod aggregator_client;
mod config;
mod statedb;

#[macro_use]
extern crate lazy_static;

use statedb::statedb_service::state_db_service_server::StateDbServiceServer;
use tokio::{
    signal::unix::{signal, SignalKind},
    spawn,
    sync::oneshot::{self, Sender},
    sync::watch,
    time,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let runtim_config = config::RuntimeConfig::from_toml("conf/base_config.toml").unwrap();
    let addr = runtim_config.addr.as_str().parse()?;
    let sdb = statedb::StateDBServiceSVC::default();

    log::info!("Launching sigterm handler");
    let (signal_tx, signal_rx) = oneshot::channel();

    let mut interval = time::interval(time::Duration::from_secs(1));
    let mut interval_client = time::interval(time::Duration::from_secs(5));
    let (send, mut recv) = watch::channel::<()>(());
    let (send_client, mut recv_client) = watch::channel::<()>(());
    spawn(wait_for_sigterm(signal_tx, send, send_client));

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = interval_client.tick() => {
                    match aggregator_client::run_client().await {
                        Ok(_) => {},
                        _ => {
                            log::info!("client quit, retrying after 5 seconds...");
                        }
                    }
                },
                _ = recv_client.changed() => {
                    log::info!("finished, break the client loop, call it a day");
                    break;
                }
            }
        }
    });

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match aggregator_client::run_prover().await {
                        Ok(_) => {
                            log::debug!("prove one task");
                        }
                        Err(e) => {
                            log::warn!("Task error: {:?}", e)
                        }
                    };
                },
                _ = recv.changed() => {
                    log::info!("finished, break the prover loop, call it a day");
                    break;
                }
            }
        }

        log::info!("finished in the walking task");
    });

    log::info!("Listening on {}", addr);
    Server::builder()
        .add_service(StateDbServiceServer::new(sdb))
        .serve_with_shutdown(addr, async {
            signal_rx.await.ok();
            log::info!("Graceful context shutdown");
        })
        .await?;

    Ok(())
}

async fn wait_for_sigterm(tx: Sender<()>, send: watch::Sender<()>, send_client: watch::Sender<()>) {
    // close prover, NOTE: should use terminate?
    let _ = signal(SignalKind::interrupt())
        .expect("failed to install signal handler")
        .recv()
        .await;
    let _ = tx.send(());
    let _ = send.send(());
    let _ = send_client.send(());
    log::info!("SIGTERM received: shutting down");
}
