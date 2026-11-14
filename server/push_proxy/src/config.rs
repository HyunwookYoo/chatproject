//! Configuration from environment variables and secret file paths only (spec 9.1).
//!
//! | Variable | Meaning |
//! |---|---|
//! | `PUSH_LISTEN` | listen address, default `127.0.0.1:8787` (plain HTTP; TLS is the reverse proxy's job) |
//! | `PUSH_KEY`, `PUSH_KEY_ID` | current handle key (64 hex chars) and its id (0–255) |
//! | `PUSH_PREV_KEY`, `PUSH_PREV_KEY_ID` | optional previous key, still opened, never sealed with |
//! | `FCM_SERVICE_ACCOUNT_FILE` | path of the FCM service account JSON |
//! | `APNS_KEY_FILE`, `APNS_KEY_ID`, `APNS_TEAM_ID`, `APNS_TOPIC`, `APNS_ENV` | APNs .p8 key path, key id, team id, bundle id, `sandbox` or `production`; all or none. None: APNs handles are refused |
//! | `PUSH_TRUST_PROXY` | `1`: take the `/handle` rate-limit IP from the last `X-Forwarded-For` entry (set only behind Caddy, which appends the real peer). Unset or `0`: the TCP peer |
//!
//! Errors name the variable, never its value.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

use data_encoding::HEXLOWER_PERMISSIVE;

const DEFAULT_LISTEN: &str = "127.0.0.1:8787";

pub struct Config {
    pub listen: SocketAddr,
    pub key_id: u8,
    key: [u8; 32],
    prev: Option<(u8, [u8; 32])>,
    pub fcm_service_account_file: PathBuf,
    pub apns: Option<ApnsConfig>,
    pub trust_proxy: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ApnsConfig {
    pub key_file: PathBuf,
    pub key_id: String,
    pub team_id: String,
    pub topic: String,
    pub env: ApnsEnv,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ApnsEnv {
    Sandbox,
    Production,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfigError {
    Missing(&'static str),
    Invalid(&'static str),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Missing(name) => write!(f, "{name} is not set"),
            ConfigError::Invalid(name) => write!(f, "{name} is invalid"),
        }
    }
}

impl std::error::Error for ConfigError {}

const APNS_VARS: [&str; 5] = [
    "APNS_KEY_FILE",
    "APNS_KEY_ID",
    "APNS_TEAM_ID",
    "APNS_TOPIC",
    "APNS_ENV",
];

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// `from_env` with the variables read through `get`.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let need = |name: &'static str| get(name).ok_or(ConfigError::Missing(name));
        let listen = get("PUSH_LISTEN")
            .unwrap_or_else(|| DEFAULT_LISTEN.to_owned())
            .parse()
            .map_err(|_| ConfigError::Invalid("PUSH_LISTEN"))?;
        let key = parse_key("PUSH_KEY", &need("PUSH_KEY")?)?;
        let key_id = parse_id("PUSH_KEY_ID", &need("PUSH_KEY_ID")?)?;
        let prev = match (get("PUSH_PREV_KEY"), get("PUSH_PREV_KEY_ID")) {
            (None, None) => None,
            (Some(k), Some(id)) => {
                let id = parse_id("PUSH_PREV_KEY_ID", &id)?;
                if id == key_id {
                    return Err(ConfigError::Invalid("PUSH_PREV_KEY_ID"));
                }
                Some((id, parse_key("PUSH_PREV_KEY", &k)?))
            }
            (Some(_), None) => return Err(ConfigError::Missing("PUSH_PREV_KEY_ID")),
            (None, Some(_)) => return Err(ConfigError::Missing("PUSH_PREV_KEY")),
        };
        let fcm_service_account_file = PathBuf::from(need("FCM_SERVICE_ACCOUNT_FILE")?);
        let apns = if APNS_VARS.iter().all(|name| get(name).is_none()) {
            None
        } else {
            Some(ApnsConfig {
                key_file: PathBuf::from(need("APNS_KEY_FILE")?),
                key_id: need("APNS_KEY_ID")?,
                team_id: need("APNS_TEAM_ID")?,
                topic: need("APNS_TOPIC")?,
                env: match need("APNS_ENV")?.as_str() {
                    "sandbox" => ApnsEnv::Sandbox,
                    "production" => ApnsEnv::Production,
                    _ => return Err(ConfigError::Invalid("APNS_ENV")),
                },
            })
        };
        let trust_proxy = match get("PUSH_TRUST_PROXY").as_deref() {
            None | Some("0") => false,
            Some("1") => true,
            Some(_) => return Err(ConfigError::Invalid("PUSH_TRUST_PROXY")),
        };
        Ok(Config {
            listen,
            key_id,
            key,
            prev,
            fcm_service_account_file,
            apns,
            trust_proxy,
        })
    }

    /// Keys the proxy opens handles with: the current one first.
    pub fn keys(&self) -> Vec<(u8, [u8; 32])> {
        std::iter::once((self.key_id, self.key))
            .chain(self.prev)
            .collect()
    }
}

fn parse_key(name: &'static str, hex: &str) -> Result<[u8; 32], ConfigError> {
    HEXLOWER_PERMISSIVE
        .decode(hex.as_bytes())
        .ok()
        .and_then(|v| <[u8; 32]>::try_from(v).ok())
        .ok_or(ConfigError::Invalid(name))
}

fn parse_id(name: &'static str, id: &str) -> Result<u8, ConfigError> {
    id.parse().map_err(|_| ConfigError::Invalid(name))
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("listen", &self.listen)
            .field("key_id", &self.key_id)
            .field("prev_key_id", &self.prev.map(|(id, _)| id))
            .field("fcm_service_account_file", &self.fcm_service_account_file)
            .field("apns", &self.apns)
            .field("trust_proxy", &self.trust_proxy)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const HEX_A: &str = "0101010101010101010101010101010101010101010101010101010101010101";
    const HEX_B: &str = "0202020202020202020202020202020202020202020202020202020202020202";

    fn load(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Config::from_lookup(|name| map.get(name).cloned())
    }

    const BASE: [(&str, &str); 3] = [
        ("PUSH_KEY", HEX_A),
        ("PUSH_KEY_ID", "1"),
        ("FCM_SERVICE_ACCOUNT_FILE", "fcm.json"),
    ];

    #[test]
    fn minimal_config_uses_defaults() {
        let c = load(&BASE).unwrap();
        assert_eq!(c.listen, DEFAULT_LISTEN.parse().unwrap());
        assert_eq!(c.keys(), vec![(1, [1; 32])]);
        assert_eq!(c.apns, None);
        assert!(!c.trust_proxy);
    }

    #[test]
    fn previous_key_and_apns_are_read() {
        let mut vars = BASE.to_vec();
        vars.extend([
            ("PUSH_PREV_KEY", HEX_B),
            ("PUSH_PREV_KEY_ID", "0"),
            ("APNS_KEY_FILE", "k.p8"),
            ("APNS_KEY_ID", "KID"),
            ("APNS_TEAM_ID", "TEAM"),
            ("APNS_TOPIC", "com.example.app"),
            ("APNS_ENV", "sandbox"),
            ("PUSH_TRUST_PROXY", "1"),
        ]);
        let c = load(&vars).unwrap();
        assert_eq!(c.keys(), vec![(1, [1; 32]), (0, [2; 32])]);
        assert!(c.trust_proxy);
        assert!(!format!("{c:?}").contains("0101"), "key in Debug");
        assert_eq!(c.apns.unwrap().env, ApnsEnv::Sandbox);
    }

    #[test]
    fn bad_values_name_the_variable_only() {
        let short = &HEX_A[..62];
        let mut vars = BASE.to_vec();
        vars[0] = ("PUSH_KEY", short);
        let err = load(&vars).unwrap_err();
        assert_eq!(err, ConfigError::Invalid("PUSH_KEY"));
        assert!(!err.to_string().contains(short));

        let mut vars = BASE.to_vec();
        vars.push(("APNS_TOPIC", "com.example.app"));
        assert_eq!(
            load(&vars).unwrap_err(),
            ConfigError::Missing("APNS_KEY_FILE")
        );

        let mut vars = BASE.to_vec();
        vars.extend([("PUSH_PREV_KEY", HEX_B), ("PUSH_PREV_KEY_ID", "1")]);
        assert_eq!(
            load(&vars).unwrap_err(),
            ConfigError::Invalid("PUSH_PREV_KEY_ID")
        );

        assert_eq!(
            load(&BASE[..2]).unwrap_err(),
            ConfigError::Missing("FCM_SERVICE_ACCOUNT_FILE")
        );
    }
}
