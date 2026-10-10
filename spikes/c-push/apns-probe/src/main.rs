//! apns-probe: sends N alert pushes (mutable-content) to one device token over APNs HTTP/2.
//! M1b device measurements (roadmap M1b, spike B device run and spike C iOS run).

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use clap::{Args, Parser, Subcommand};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use reqwest::header::HeaderValue;
use serde::Serialize;
use serde_json::{Value, json};

/// APNs limit for everything except VoIP (5120).
const MAX_PAYLOAD: usize = 4096;
/// Apple: mint a new token no more than once per 20 minutes, and keep none longer than 60.
const JWT_REFRESH_AFTER: Duration = Duration::from_secs(45 * 60);

#[derive(Parser)]
#[command(name = "apns-probe", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Send --count pushes, one every --interval-ms.
    Send(SendArgs),
}

#[derive(Args)]
struct SendArgs {
    /// AuthKey_XXXXXXXXXX.p8 (PKCS#8 PEM). Keep it in spikes/c-push/secrets/.
    #[arg(long)]
    key: PathBuf,
    /// 10-character APNs key id.
    #[arg(long)]
    key_id: String,
    /// 10-character Apple team id.
    #[arg(long)]
    team_id: String,
    /// apns-topic: the app's bundle id, never the extension's.
    #[arg(long)]
    topic: String,
    /// Device token as hex (the probe screen's "토큰 복사").
    #[arg(long)]
    token: String,
    #[arg(long, default_value_t = 1)]
    count: u32,
    #[arg(long, default_value_t = 1000)]
    interval_ms: u64,
    /// Number of the first push; continue a series across runs.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    start_seq: u32,
    /// Base64 ciphertexts, one per line (Fixture/ciphertexts.txt). Without it, no `e` is sent.
    #[arg(long)]
    ciphertexts: Option<PathBuf>,
    /// apns-expiration = now + this many seconds. 0 means "attempt once, do not store".
    #[arg(long, default_value_t = 3600)]
    expiration_secs: u64,
    /// api.sandbox.push.apple.com (Xcode debug builds only). TestFlight needs production.
    #[arg(long)]
    sandbox: bool,
}

#[derive(Serialize)]
struct Claims<'a> {
    iss: &'a str,
    iat: u64,
}

fn host(sandbox: bool) -> &'static str {
    if sandbox { "https://api.sandbox.push.apple.com" } else { "https://api.push.apple.com" }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("clock before 1970").as_millis() as u64
}

fn make_jwt(key: &EncodingKey, key_id: &str, team_id: &str, iat_secs: u64) -> Result<String> {
    let mut header = Header::new(Algorithm::ES256);
    header.typ = None; // Apple documents only `alg` and `kid`
    header.kid = Some(key_id.to_owned());
    Ok(encode(&header, &Claims { iss: team_id, iat: iat_secs }, key)?)
}

/// Custom keys are peers of `aps`; Apple ignores custom keys inside `aps`.
fn build_payload(seq: u32, sent_at_ms: u64, ciphertext: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut v = json!({
        "aps": {
            "alert": { "title": "측정", "body": format!("알림 {seq}번") },
            "mutable-content": 1
        },
        "seq": seq,
        "sent_at": sent_at_ms
    });
    if let Some(ct) = ciphertext {
        v["e"] = Value::String(STANDARD.encode(ct));
    }
    let bytes = serde_json::to_vec(&v)?;
    if bytes.len() > MAX_PAYLOAD {
        bail!("payload is {} bytes, APNs limit is {MAX_PAYLOAD}", bytes.len());
    }
    Ok(bytes)
}

fn read_ciphertexts(path: &Path) -> Result<Vec<Vec<u8>>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let lines = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| STANDARD.decode(l.trim()).context("ciphertext line is not base64"))
        .collect::<Result<Vec<_>>>()?;
    if lines.is_empty() {
        bail!("{} has no ciphertext lines", path.display());
    }
    Ok(lines)
}

/// Push `seq` (1-based) carries line `(seq - 1) mod n`.
fn ciphertext_for(lines: &[Vec<u8>], seq: u32) -> Option<&[u8]> {
    if lines.is_empty() {
        return None;
    }
    Some(&lines[(seq as usize - 1) % lines.len()])
}

#[tokio::main]
async fn main() -> Result<()> {
    let Cmd::Send(a) = Cli::parse().cmd;
    let pem = std::fs::read(&a.key).with_context(|| format!("read {}", a.key.display()))?;
    let key = EncodingKey::from_ec_pem(&pem).context("not a PKCS#8 EC private key (.p8)")?;
    let lines = match &a.ciphertexts {
        Some(path) => read_ciphertexts(path)?,
        None => Vec::new(),
    };
    let url = format!("{}/3/device/{}", host(a.sandbox), a.token);
    let client = reqwest::Client::builder()
        .http2_prior_knowledge() // APNs speaks HTTP/2 only
        .timeout(Duration::from_secs(30))
        .build()?;

    let mut minted = Instant::now();
    let mut jwt = make_jwt(&key, &a.key_id, &a.team_id, now_ms() / 1000)?;
    let (mut ok, mut failed) = (0u32, 0u32);
    for seq in a.start_seq..a.start_seq + a.count {
        if minted.elapsed() >= JWT_REFRESH_AFTER {
            jwt = make_jwt(&key, &a.key_id, &a.team_id, now_ms() / 1000)?;
            minted = Instant::now();
        }
        let sent_at = now_ms();
        let body = build_payload(seq, sent_at, ciphertext_for(&lines, seq))?;
        let expiration = if a.expiration_secs == 0 { 0 } else { sent_at / 1000 + a.expiration_secs };
        let mut auth = HeaderValue::from_str(&format!("bearer {jwt}"))?;
        auth.set_sensitive(true);

        let started = Instant::now();
        let response = client
            .post(&url)
            .header("authorization", auth)
            .header("apns-topic", &a.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .header("apns-expiration", expiration.to_string())
            .header("apns-id", uuid::Uuid::new_v4().to_string())
            .body(body.clone())
            .send()
            .await;
        let rtt = started.elapsed().as_millis();
        match response {
            Err(e) => {
                failed += 1;
                // reqwest's Display hides the cause (TLS, DNS, proxy, h2), so print the whole chain.
                println!("seq={seq} bytes={} NETWORK-ERROR rtt_ms={rtt} {:#}", body.len(), anyhow::Error::from(e));
            }
            Ok(r) => {
                let status = r.status();
                let apns_id = r.headers().get("apns-id").and_then(|v| v.to_str().ok()).unwrap_or("-").to_owned();
                // Empty on 200; otherwise {"reason": ..., "timestamp"?: ...}.
                let text = match r.text().await {
                    Ok(text) => text,
                    Err(e) => format!("(body read failed: {:#})", anyhow::Error::from(e)),
                };
                if status.is_success() { ok += 1 } else { failed += 1 }
                println!("seq={seq} bytes={} status={} apns-id={apns_id} rtt_ms={rtt} {text}", body.len(), status.as_u16());
            }
        }
        if seq + 1 < a.start_seq + a.count {
            tokio::time::sleep(Duration::from_millis(a.interval_ms)).await;
        }
    }
    println!("done: ok={ok} failed={failed}");
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use p256::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};

    fn throwaway_keypair() -> (String, String) {
        let sk = p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
        let private = sk.to_pkcs8_pem(LineEnding::LF).unwrap().to_string();
        let public = sk.public_key().to_public_key_pem(LineEnding::LF).unwrap();
        (private, public)
    }

    #[test]
    fn jwt_header_and_claims_match_apple_doc() {
        let (private, public) = throwaway_keypair();
        let key = EncodingKey::from_ec_pem(private.as_bytes()).unwrap();
        let jwt = make_jwt(&key, "ABC123DEFG", "DEF123GHIJ", 1_700_000_000).unwrap();

        let header = jsonwebtoken::decode_header(&jwt).unwrap();
        assert_eq!(header.alg, Algorithm::ES256);
        assert_eq!(header.kid.as_deref(), Some("ABC123DEFG"));
        assert!(header.typ.is_none());

        let parts: Vec<&str> = jwt.split('.').collect();
        let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims, json!({ "iss": "DEF123GHIJ", "iat": 1_700_000_000u64 }));

        let mut validation = jsonwebtoken::Validation::new(Algorithm::ES256);
        validation.required_spec_claims.clear();
        validation.validate_exp = false;
        let decoding = jsonwebtoken::DecodingKey::from_ec_pem(public.as_bytes()).unwrap();
        jsonwebtoken::decode::<Value>(&jwt, &decoding, &validation).unwrap();
    }

    #[test]
    fn payload_shape() {
        let p: Value = serde_json::from_slice(&build_payload(7, 1_700_000_000_123, Some(&[1, 2, 3])).unwrap()).unwrap();
        assert_eq!(p["aps"]["mutable-content"], 1);
        assert_eq!(p["aps"]["alert"]["title"], "측정");
        assert_eq!(p["aps"]["alert"]["body"], "알림 7번");
        assert_eq!((p["seq"].as_u64(), p["sent_at"].as_u64()), (Some(7), Some(1_700_000_000_123)));
        assert_eq!(STANDARD.decode(p["e"].as_str().unwrap()).unwrap(), [1, 2, 3]);
        assert!(p["aps"].get("seq").is_none(), "custom keys must be peers of aps");
        let without: Value = serde_json::from_slice(&build_payload(1, 1, None).unwrap()).unwrap();
        assert!(without.get("e").is_none());
    }

    #[test]
    fn payload_size_boundary_is_4096() {
        let fits = |n: usize| build_payload(1, 1_700_000_000_000, Some(&vec![0xAB; n])).map(|b| b.len());
        let max = (0..4096).rev().find(|&n| fits(n).is_ok()).unwrap();
        assert!(fits(max).unwrap() <= MAX_PAYLOAD);
        assert!(fits(max + 1).is_err());
        assert!((2900..3100).contains(&max), "largest raw ciphertext was {max}");
    }

    #[test]
    fn ciphertext_for_seq_cycles_through_lines() {
        let lines = vec![vec![1u8], vec![2u8], vec![3u8]];
        assert_eq!(ciphertext_for(&lines, 1), Some(&[1u8][..]));
        assert_eq!(ciphertext_for(&lines, 3), Some(&[3u8][..]));
        assert_eq!(ciphertext_for(&lines, 4), Some(&[1u8][..]));
        assert_eq!(ciphertext_for(&[], 1), None);
    }

    #[test]
    fn empty_ciphertext_file_is_rejected() {
        let dir = std::env::temp_dir().join(format!("apns_probe_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty.txt");
        std::fs::write(&path, "\n\n").unwrap();
        assert!(read_ciphertexts(&path).is_err());
        std::fs::write(&path, "AQID\nBAUG\n").unwrap();
        assert_eq!(read_ciphertexts(&path).unwrap(), vec![vec![1, 2, 3], vec![4, 5, 6]]);
    }

    #[test]
    fn production_is_the_default_endpoint() {
        assert_eq!(host(false), "https://api.push.apple.com");
        assert_eq!(host(true), "https://api.sandbox.push.apple.com");
    }

    #[test]
    fn start_seq_must_be_at_least_one() {
        let parse = |seq: &str| {
            Cli::try_parse_from([
                "apns-probe", "send", "--key", "k.p8", "--key-id", "KEY", "--team-id", "TEAM",
                "--topic", "dev.chatproject.chatapp", "--token", "00", "--start-seq", seq,
            ])
        };
        assert!(parse("0").is_err());
        assert!(parse("1").is_ok());
    }
}
