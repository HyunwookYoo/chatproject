//! `push_proxy`: plain HTTP on `PUSH_LISTEN`; TLS is the reverse proxy's job
//! (spec 9.1). Configuration: see `Config` and README.md.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use push_proxy::{
    APNS_PRODUCTION, APNS_SANDBOX, ApnsConfig, ApnsEnv, ApnsUpstream, AppState, Config,
    FCM_ENDPOINT, FcmUpstream, GOOGLE_TOKEN_URL, Upstream, Url, router, serve,
};

#[tokio::main]
async fn main() -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("push_proxy: {e}");
            return ExitCode::FAILURE;
        }
    };
    let senders = fcm_sender(&config).and_then(|fcm| {
        let apns = config.apns.as_ref().map(apns_sender).transpose()?;
        Ok((fcm, apns))
    });
    let (fcm, apns) = match senders {
        Ok(senders) => senders,
        Err(e) => {
            eprintln!("push_proxy: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut state = AppState::new(config.keys(), config.key_id, Arc::new(fcm));
    state.apns = apns.map(|a| Arc::new(a) as Arc<dyn Upstream>);
    state.trust_proxy = config.trust_proxy;
    let listener = match tokio::net::TcpListener::bind(config.listen).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("push_proxy: cannot listen on {}: {e}", config.listen);
            return ExitCode::FAILURE;
        }
    };
    let header_timeout = state.limits.header_timeout;
    serve(listener, router(state), header_timeout).await;
    ExitCode::SUCCESS
}

/// Errors name the variable, never the file's content.
fn fcm_sender(config: &Config) -> Result<FcmUpstream, String> {
    const VAR: &str = "FCM_SERVICE_ACCOUNT_FILE";
    let json = read_secret(&config.fcm_service_account_file, VAR)?;
    FcmUpstream::new(&json, url(FCM_ENDPOINT)?, url(GOOGLE_TOKEN_URL)?)
        .map_err(|e| format!("{VAR}: {e}"))
}

fn apns_sender(apns: &ApnsConfig) -> Result<ApnsUpstream, String> {
    let pem = read_secret(&apns.key_file, "APNS_KEY_FILE")?;
    let endpoint = url(match apns.env {
        ApnsEnv::Production => APNS_PRODUCTION,
        ApnsEnv::Sandbox => APNS_SANDBOX,
    })?;
    ApnsUpstream::new(&pem, &apns.key_id, &apns.team_id, &apns.topic, endpoint)
        .map_err(|e| e.to_string())
}

fn read_secret(path: &Path, var: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {var}: {}", e.kind()))
}

fn url(text: &str) -> Result<Url, String> {
    Url::parse(text).map_err(|_| format!("bad built-in URL {text}"))
}
