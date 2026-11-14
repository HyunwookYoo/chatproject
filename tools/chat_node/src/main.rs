use chat_core::account::{
    Secret32, StoreId,
    storage::KeyProtector,
};
use chat_core::{CoreConfig, CoreError, CoreSlot, ErrorCode, EventBus, NetworkOp};
use clap::{Parser, Subcommand};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};

#[derive(Parser)]
#[command(about = "Local relay and debug core participant for ChatProject tests")]
struct Cli {
    #[command(subcommand)]
    command: Role,
}

#[derive(Subcommand)]
enum Role {
    Relay {
        #[arg(long, default_value = "127.0.0.1:3340")]
        bind: SocketAddr,
    },
    Puppet {
        #[arg(long)]
        control_url: String,
        #[arg(long)]
        name: String,
        #[arg(long, value_parser = absolute_dir)]
        data_dir: PathBuf,
        /// Also open the account in the data directory (an account participant, as the apps
        /// are), once the controller's configuration is in place.
        #[arg(long)]
        account: bool,
    },
}

/// The node's account key wrapping. Harness only: the store key is kept XOR-masked next to
/// the store, bound to its public store ID (no OS key store on a test node).
struct HarnessProtector;

impl KeyProtector for HarnessProtector {
    fn protect(&self, id: StoreId, key: &Secret32) -> Result<Vec<u8>, CoreError> {
        let mut blob = id.0.to_vec();
        blob.extend(key.as_bytes().iter().map(|b| b ^ 0x5c));
        Ok(blob)
    }

    fn unprotect(&self, id: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
        if blob.len() != 48 || blob[..16] != id.0 {
            return Err(CoreError::new(
                ErrorCode::StorageLocked,
                "harness key unavailable",
            ));
        }
        Ok(Secret32::from(std::array::from_fn(|i| blob[16 + i] ^ 0x5c)))
    }

    fn delete(&self, _: StoreId) -> Result<(), CoreError> {
        Ok(())
    }
}

fn absolute_dir(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err("data-dir must be absolute".into())
    }
}

fn shutdown_signal() -> std::io::Result<impl std::future::Future<Output = ()>> {
    #[cfg(windows)]
    {
        let mut interrupt = tokio::signal::windows::ctrl_c()?;
        let mut ctrl_break = tokio::signal::windows::ctrl_break()?;
        Ok(async move {
            tokio::select! {
                _ = interrupt.recv() => (),
                _ = ctrl_break.recv() => (),
            }
        })
    }
    #[cfg(unix)]
    {
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        Ok(async move {
            interrupt.recv().await;
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    // Install handlers before startup and before the relay publishes readiness.
    let shutdown = shutdown_signal()?;
    tokio::pin!(shutdown);
    match cli.command {
        Role::Relay { bind } => {
            if !bind.ip().is_loopback() {
                return Err("relay bind must be loopback".into());
            }
            let mut config = iroh_relay::server::ServerConfig::default();
            config.relay = Some(iroh_relay::server::RelayConfig::new(bind));
            config.quic = None;
            config.metrics_addr = None;
            let server = iroh_relay::server::Server::spawn(config).await?;
            println!("{}", serde_json::json!({"type":"ready","role":"relay"}));
            shutdown.await;
            server.shutdown().await?;
        }
        Role::Puppet {
            control_url,
            name,
            data_dir,
            account,
        } => {
            let slot = CoreSlot::new();
            slot.start(CoreConfig { data_dir }, &EventBus::new())?;
            // Core construction is stopped; the hook receives Local configuration
            // before a controller may issue network_start.
            tokio::select! {
                result = slot.test_hooks_start(control_url, name) => {
                    result?;
                    if account {
                        slot.open_account(Arc::new(HarnessProtector))?;
                    }
                    shutdown.as_mut().await;
                }
                _ = shutdown.as_mut() => (),
            }
            slot.network()?.execute(NetworkOp::Stop).await?;
        }
    }
    Ok(())
}
