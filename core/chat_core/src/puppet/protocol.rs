//! Debug control wire. IDs must increase strictly from a positive u64 on each
//! connection. Event sequences instead belong to the owning core instance.
use crate::{CoreError, ErrorCode, NetworkInfo, NetworkOp, NetworkReply, PeerInfo};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL: &str = "chat-test-hooks:v1";
pub const RELAY: &str = "http://127.0.0.1:3340";
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Incoming {
    Config {
        #[serde(deserialize_with = "Option::deserialize")]
        relay: Option<String>,
        #[serde(default)]
        records: Option<String>,
    },
    Cmd {
        id: u64,
        op: String,
        args: Value,
    },
}
impl std::fmt::Debug for Incoming {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Incoming([redacted])")
    }
}
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outgoing<'a> {
    Hello {
        protocol: &'a str,
        name: &'a str,
        instance: &'a str,
        relay: &'a str,
    },
    Result {
        id: u64,
        #[serde(flatten)]
        result: Outcome,
    },
    Event {
        seq: u64,
        instance: &'a str,
        event: crate::CoreEvent,
    },
}
#[derive(Serialize)]
#[serde(untagged)]
pub enum Outcome {
    Success { ok: bool, value: ValueReply },
    Failure { ok: bool, error: WireError },
}
#[derive(Serialize)]
#[serde(untagged)]
pub enum ValueReply {
    Info(NetworkInfo),
    Peer(PeerInfo),
    Done,
    Account(super::account::SecretJson),
}
#[derive(Serialize)]
pub struct WireError {
    code: ErrorCode,
    detail: String,
}
impl From<Result<NetworkReply, CoreError>> for Outcome {
    fn from(result: Result<NetworkReply, CoreError>) -> Self {
        match result {
            Ok(reply) => Self::Success {
                ok: true,
                value: match reply {
                    NetworkReply::Info(info) => ValueReply::Info(info),
                    NetworkReply::Peer(peer) => ValueReply::Peer(peer),
                    NetworkReply::Done => ValueReply::Done,
                },
            },
            Err(error) => Self::Failure {
                ok: false,
                error: WireError {
                    code: error.code,
                    detail: error.detail,
                },
            },
        }
    }
}
impl Outcome {
    pub fn account(result: Result<Value, CoreError>) -> Self {
        match result {
            Ok(value) => Self::Success { ok: true, value: ValueReply::Account(super::account::SecretJson(value)) },
            Err(error) => Self::Failure { ok: false, error: WireError { code: error.code, detail: "Account action failed".into() } },
        }
    }
}
pub fn invalid() -> CoreError {
    CoreError::new(ErrorCode::InvalidConfig, "invalid control record")
}
pub fn parse(text: &str) -> Result<Incoming, CoreError> {
    serde_json::from_str(text).map_err(|_| invalid())
}
pub fn validate_control(value: &str) -> Result<(), CoreError> {
    validate_url(value, "ws", 47000)
}
pub fn validate_relay(value: &str) -> Result<(), CoreError> {
    validate_url(value, "http", 3340)
}
fn validate_url(value: &str, scheme: &str, port: u16) -> Result<(), CoreError> {
    // Literal addresses prevent DNS lookup before isolation is configured.
    let allowed = [
        format!("{scheme}://127.0.0.1:{port}"),
        format!("{scheme}://[::1]:{port}"),
    ];
    if allowed
        .iter()
        .any(|url| value == url || value == format!("{url}/"))
    {
        Ok(())
    } else {
        Err(invalid())
    }
}
#[derive(Default)]
pub struct RequestIds(u64);
impl RequestIds {
    pub fn accept(&mut self, id: u64) -> Result<(), CoreError> {
        if id <= self.0 {
            return Err(invalid());
        }
        self.0 = id;
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    target: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Peer {
    peer_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Text {
    peer_id: String,
    text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Offline {
    offline: bool,
}
fn args<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, CoreError> {
    if !value.is_object() {
        return Err(invalid());
    }
    serde_json::from_value(value).map_err(|_| invalid())
}
pub fn command(op: &str, value: Value) -> Result<NetworkOp, CoreError> {
    Ok(match op {
        "network_start" => {
            args::<Empty>(value)?;
            NetworkOp::Start
        }
        "network_info" => {
            args::<Empty>(value)?;
            NetworkOp::Info
        }
        "network_stop" => {
            args::<Empty>(value)?;
            NetworkOp::Stop
        }
        "connect_peer" => NetworkOp::Connect {
            target: args::<Target>(value)?.target,
        },
        "disconnect_peer" => NetworkOp::Disconnect {
            peer_id: args::<Peer>(value)?.peer_id,
        },
        "send_probe_text" => {
            let Text { peer_id, text } = args(value)?;
            NetworkOp::Send { peer_id, text }
        }
        "set_offline" => NetworkOp::SetOffline {
            offline: args::<Offline>(value)?.offline,
        },
        _ => {
            return Err(CoreError::new(
                ErrorCode::NotAvailable,
                "unknown control command",
            ));
        }
    })
}
#[cfg(test)]
mod account_config_tests {
    #[test]
    fn local_account_services_configuration_is_accepted_before_open() {
        assert!(super::parse(r#"{"type":"config","relay":null,"records":"ws://127.0.0.1:47000/records"}"#).is_ok());
    }
}
