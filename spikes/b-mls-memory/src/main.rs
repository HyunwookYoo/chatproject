//! Spike B: can an iOS Notification Service Extension decrypt one OpenMLS group message
//! within ~15 MB? Desktop stand-in: count Rust heap bytes while a fresh provider loads
//! receiver R's persisted group state and decrypts one application message.
//!
//! `cargo run --release` runs every (N, scenario) case in its own child process and prints
//! a Markdown table. `cargo run --release -- <N> <default|rev3>` runs a single case.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;

// --- Counting allocator: current and peak live heap bytes ------------------------------

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(bytes: usize) {
    let now = CURRENT.fetch_add(bytes, Relaxed) + bytes;
    PEAK.fetch_max(now, Relaxed);
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grow(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            grow(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
        CURRENT.fetch_sub(layout.size(), Relaxed);
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, layout, new_size) };
        if !q.is_null() {
            // Conservative: a moving realloc briefly holds both the old and the new block.
            grow(new_size);
            CURRENT.fetch_sub(layout.size(), Relaxed);
        }
        q
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

// --- MLS helpers ------------------------------------------------------------------------

const CS: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;
const PLAINTEXT: [u8; 1024] = [0x42; 1024];

struct Client {
    provider: OpenMlsRustCrypto,
    signer: SignatureKeyPair,
    credential: CredentialWithKey,
}

impl Client {
    fn new(name: &str) -> Self {
        let provider = OpenMlsRustCrypto::default();
        let signer = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
        signer.store(provider.storage()).unwrap();
        let credential = CredentialWithKey {
            credential: BasicCredential::new(name.as_bytes().to_vec()).into(),
            signature_key: signer.public().into(),
        };
        Self {
            provider,
            signer,
            credential,
        }
    }

    fn key_package(&self) -> KeyPackage {
        let bundle = KeyPackage::builder()
            .build(CS, &self.provider, &self.signer, self.credential.clone())
            .unwrap();
        bundle.key_package().clone()
    }

    /// Encrypts a 1 KiB application message and returns its wire bytes.
    fn send(&self, group: &mut MlsGroup) -> Vec<u8> {
        let msg = group.create_message(&self.provider, &self.signer, &PLAINTEXT);
        to_wire(&msg.unwrap())
    }
}

/// Every message goes through its wire encoding, as it would over a real transport.
fn to_wire(msg: &MlsMessageOut) -> Vec<u8> {
    msg.tls_serialize_detached().unwrap()
}

fn process(
    group: &mut MlsGroup,
    provider: &OpenMlsRustCrypto,
    wire: &[u8],
) -> ProcessedMessageContent {
    let msg = MlsMessageIn::tls_deserialize_exact(wire).unwrap();
    let msg = msg.try_into_protocol_message().unwrap();
    group.process_message(provider, msg).unwrap().into_content()
}

fn receive_commit(group: &mut MlsGroup, provider: &OpenMlsRustCrypto, commit: &MlsMessageOut) {
    match process(group, provider, &to_wire(commit)) {
        ProcessedMessageContent::StagedCommitMessage(staged) => {
            group.merge_staged_commit(provider, *staged).unwrap()
        }
        _ => panic!("expected a commit"),
    }
}

fn decrypt(group: &mut MlsGroup, provider: &OpenMlsRustCrypto, wire: &[u8]) -> Vec<u8> {
    match process(group, provider, wire) {
        ProcessedMessageContent::ApplicationMessage(app) => app.into_bytes(),
        _ => panic!("expected an application message"),
    }
}

// --- One benchmark case -----------------------------------------------------------------

fn run_case(n: usize, scenario: &str) {
    let create_config = match scenario {
        "default" => MlsGroupCreateConfig::default(),
        "rev3" => MlsGroupCreateConfig::builder()
            .max_past_epochs(5)
            .sender_ratchet_configuration(SenderRatchetConfiguration::new(200, 1000))
            .build(),
        _ => panic!("scenario must be `default` or `rev3`"),
    };
    let join_config = create_config.join_config().clone();

    // S creates the group and adds R plus N-2 throwaway members, up to 50 per commit.
    // A throwaway provider is dropped right after it makes its KeyPackage: those members
    // never process anything.
    let s = Client::new("S");
    let r = Client::new("R");
    let mut s_group =
        MlsGroup::new(&s.provider, &s.signer, &create_config, s.credential.clone()).unwrap();
    let mut key_packages = vec![r.key_package()];
    key_packages.extend((2..n).map(|i| Client::new(&format!("M{i}")).key_package()));

    let mut r_group: Option<MlsGroup> = None;
    for batch in key_packages.chunks(50) {
        let (commit, welcome, _) = s_group.add_members(&s.provider, &s.signer, batch).unwrap();
        s_group.merge_pending_commit(&s.provider).unwrap();
        match &mut r_group {
            Some(group) => receive_commit(group, &r.provider, &commit),
            None => {
                let welcome = MlsMessageIn::tls_deserialize_exact(to_wire(&welcome)).unwrap();
                let MlsMessageBodyIn::Welcome(welcome) = welcome.extract() else {
                    panic!("expected a welcome")
                };
                // Default config has no ratchet-tree extension, so the tree goes out of band.
                let tree = s_group.export_ratchet_tree().into();
                let staged =
                    StagedWelcome::new_from_welcome(&r.provider, &join_config, welcome, Some(tree))
                        .unwrap();
                r_group = Some(staged.into_group(&r.provider).unwrap());
            }
        }
    }
    let mut r_group = r_group.unwrap();

    // Traffic as design 6.3 expects (same for both scenarios; only the config differs):
    // 6 more epochs that R processes, then 200 messages of which R decrypts only the last.
    for _ in 0..6 {
        let bundle = s_group
            .self_update(&s.provider, &s.signer, LeafNodeParameters::default())
            .unwrap();
        s_group.merge_pending_commit(&s.provider).unwrap();
        receive_commit(&mut r_group, &r.provider, bundle.commit());
    }
    let mut last = Vec::new();
    for _ in 0..200 {
        last = s.send(&mut s_group);
    }
    assert_eq!(decrypt(&mut r_group, &r.provider, &last), PLAINTEXT);

    // The new message the NSE has to decrypt.
    let wire = s.send(&mut s_group);
    let group_id = s_group.group_id().clone();

    // (a) Persisted state: copy of R's storage key-value map.
    let snapshot = r.provider.storage().values.read().unwrap().clone();
    let state_bytes: usize = snapshot.iter().map(|(k, v)| k.len() + v.len()).sum();

    // (b), (c) A fresh provider over a copy of R's storage loads the group and decrypts.
    // Writes land in the copy and are dropped: the NSE keeps no state changes.
    // Repeated 5 times on fresh copies; the first run is the "cold" one.
    let mut process_peak = PEAK.load(Relaxed);
    let mut peak_delta = 0;
    let mut times_ms = Vec::new();
    for _ in 0..5 {
        let nse = OpenMlsRustCrypto::default();
        *nse.storage().values.write().unwrap() = snapshot.clone();

        let base = CURRENT.load(Relaxed);
        PEAK.store(base, Relaxed);
        let start = Instant::now();
        let mut group = MlsGroup::load(nse.storage(), &group_id).unwrap().unwrap();
        let plaintext = decrypt(&mut group, &nse, &wire);
        let elapsed = start.elapsed();
        let peak = PEAK.load(Relaxed);

        assert_eq!(plaintext, PLAINTEXT);
        peak_delta = peak_delta.max(peak - base);
        process_peak = process_peak.max(peak);
        times_ms.push(elapsed.as_secs_f64() * 1e3);
    }
    let first_ms = times_ms[0];
    times_ms.sort_by(f64::total_cmp);

    let kib = |b: usize| b as f64 / 1024.0;
    println!(
        "| {n} | {scenario} | {:.1} | {:.1} | {:.2} / {:.2} | {:.1} |",
        kib(state_bytes),
        kib(peak_delta),
        first_ms,
        times_ms[2],
        kib(process_peak),
    );

    // State breakdown by storage label (key prefix), largest first.
    let mut by_label = BTreeMap::<String, usize>::new();
    for (k, v) in &snapshot {
        let label = k
            .iter()
            .take_while(|b| b.is_ascii_alphabetic())
            .map(|&b| b as char);
        *by_label.entry(label.collect()).or_default() += k.len() + v.len();
    }
    let mut by_label: Vec<_> = by_label.into_iter().collect();
    by_label.sort_by(|a, b| b.1.cmp(&a.1));
    let parts: Vec<_> = by_label
        .iter()
        .map(|(l, b)| format!("{l} {:.1}", kib(*b)))
        .collect();
    println!(
        "{n} {scenario} ({} keys): {}",
        snapshot.len(),
        parts.join(", ")
    );
}

// --- Driver -----------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let [_, n, scenario] = args.as_slice() {
        run_case(n.parse().expect("N must be a number"), scenario);
        return;
    }

    let exe = std::env::current_exe().unwrap();
    let mut rows = Vec::new();
    let mut breakdowns = Vec::new();
    for scenario in ["default", "rev3"] {
        for n in [10, 50, 100, 200] {
            let out = Command::new(&exe)
                .arg(n.to_string())
                .arg(scenario)
                .output()
                .unwrap();
            let stdout = String::from_utf8(out.stdout).unwrap();
            assert!(
                out.status.success(),
                "{n} {scenario} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let mut lines = stdout.lines();
            rows.push(lines.next().unwrap().to_string());
            breakdowns.extend(lines.map(str::to_string));
        }
    }

    println!(
        "Release binary: {} bytes\n",
        std::fs::metadata(&exe).unwrap().len()
    );
    println!(
        "| N | scenario | (a) state KiB | (b) load+decrypt peak delta KiB | (c) ms first / median of 5 | (d) process peak KiB |"
    );
    println!("|---|---|---|---|---|---|");
    rows.iter().for_each(|row| println!("{row}"));
    println!("\nState breakdown, KiB by storage label:");
    breakdowns.iter().for_each(|line| println!("- {line}"));
}
