//! Configuration from environment variables only.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `QUEUE_LISTEN` | listen address, default `127.0.0.1:8788` (plain HTTP; TLS is the reverse proxy's job) |
//! | `QUEUE_DB` | SQLite file, default `/var/lib/queue_server/queue.db` |
//! | `QUEUE_MAX_BYTES` | used bytes of the file past which `SEND` is `limited`, default 2 GiB |
//! | `QUEUE_TRUST_PROXY` | `1`: per-client limits use the last `X-Forwarded-For` entry (set only behind Caddy, which appends the real peer). Unset or `0`: the TCP peer |
//!
//! Errors name the variable, never its value.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

use crate::limit::DEFAULT_MAX_BYTES;

const DEFAULT_LISTEN: &str = "127.0.0.1:8788";
const DEFAULT_DB: &str = "/var/lib/queue_server/queue.db";

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    pub db: PathBuf,
    pub max_bytes: u64,
    pub trust_proxy: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfigError {
    Invalid(&'static str),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Invalid(name) => write!(f, "{name} is invalid"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// `from_env` with the variables read through `get`.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let listen = get("QUEUE_LISTEN")
            .unwrap_or_else(|| DEFAULT_LISTEN.to_owned())
            .parse()
            .map_err(|_| ConfigError::Invalid("QUEUE_LISTEN"))?;
        let db = match get("QUEUE_DB") {
            Some(path) if path.is_empty() => return Err(ConfigError::Invalid("QUEUE_DB")),
            Some(path) => PathBuf::from(path),
            None => PathBuf::from(DEFAULT_DB),
        };
        let max_bytes = match get("QUEUE_MAX_BYTES") {
            None => DEFAULT_MAX_BYTES,
            Some(v) => v
                .parse()
                .map_err(|_| ConfigError::Invalid("QUEUE_MAX_BYTES"))?,
        };
        let trust_proxy = match get("QUEUE_TRUST_PROXY").as_deref() {
            None | Some("0") => false,
            Some("1") => true,
            Some(_) => return Err(ConfigError::Invalid("QUEUE_TRUST_PROXY")),
        };
        Ok(Config {
            listen,
            db,
            max_bytes,
            trust_proxy,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn load(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Config::from_lookup(|name| map.get(name).cloned())
    }

    #[test]
    fn empty_config_uses_defaults() {
        let c = load(&[]).unwrap();
        assert_eq!(c.listen, DEFAULT_LISTEN.parse().unwrap());
        assert_eq!(c.db, PathBuf::from(DEFAULT_DB));
        assert_eq!(c.max_bytes, 2_147_483_648);
        assert!(!c.trust_proxy);
    }

    #[test]
    fn values_are_read() {
        let c = load(&[
            ("QUEUE_LISTEN", "0.0.0.0:9000"),
            ("QUEUE_DB", "q.db"),
            ("QUEUE_MAX_BYTES", "1000"),
            ("QUEUE_TRUST_PROXY", "1"),
        ])
        .unwrap();
        assert_eq!(c.listen, "0.0.0.0:9000".parse().unwrap());
        assert_eq!(c.db, PathBuf::from("q.db"));
        assert_eq!(c.max_bytes, 1000);
        assert!(c.trust_proxy);
    }

    #[test]
    fn bad_values_name_the_variable_only() {
        for (name, value) in [
            ("QUEUE_LISTEN", "nowhere:1"),
            ("QUEUE_DB", ""),
            ("QUEUE_MAX_BYTES", "-5"),
            ("QUEUE_MAX_BYTES", "2GiB"),
            ("QUEUE_TRUST_PROXY", "yes"),
        ] {
            let err = load(&[(name, value)]).unwrap_err();
            assert_eq!(err, ConfigError::Invalid(name));
            assert!(value.is_empty() || !err.to_string().contains(value));
        }
    }
}
