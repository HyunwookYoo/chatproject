use chat_core::{CoreConfig, CoreSlot, EventBus, NetworkOp};
use clap::{Parser, Subcommand};
use std::{net::SocketAddr, path::PathBuf};

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
    },
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
        } => {
            let slot = CoreSlot::new();
            slot.start(CoreConfig { data_dir }, &EventBus::new())?;
            // Core construction is stopped; the hook receives Local configuration
            // before a controller may issue network_start.
            tokio::select! {
                result = slot.test_hooks_start(control_url, name) => {
                    result?;
                    shutdown.as_mut().await;
                }
                _ = shutdown.as_mut() => (),
            }
            slot.network()?.execute(NetworkOp::Stop).await?;
        }
    }
    Ok(())
}
