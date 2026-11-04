//! Host-side memory proxy for the notification extension.
//!
//! Usage: probe_peak <nse_state.bin> <ciphertexts.txt>
//! Prints the process's peak working set and peak commit at start, after the first
//! `probe_decrypt` and after all of them. Windows and Linux only (no extra crates). This is a
//! proxy: iOS `phys_footprint` of the real extension is measured by the M1b TestFlight probe.

use std::fs;

use base64::{Engine as _, engine::general_purpose::STANDARD};

/// (peak working set, peak commit) in bytes.
#[cfg(windows)]
fn peaks() -> (u64, u64) {
    #[repr(C)]
    struct Counters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set: usize,
        working_set: usize,
        quota_peak_paged_pool: usize,
        quota_paged_pool: usize,
        quota_peak_non_paged_pool: usize,
        quota_non_paged_pool: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(process: isize, counters: *mut Counters, cb: u32) -> i32;
    }
    let mut counters = Counters {
        cb: size_of::<Counters>() as u32,
        page_fault_count: 0,
        peak_working_set: 0,
        working_set: 0,
        quota_peak_paged_pool: 0,
        quota_paged_pool: 0,
        quota_peak_non_paged_pool: 0,
        quota_non_paged_pool: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
    };
    // SAFETY: `counters` is a valid PROCESS_MEMORY_COUNTERS of the size passed.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    assert_ne!(ok, 0, "GetProcessMemoryInfo failed");
    (
        counters.peak_working_set as u64,
        counters.peak_pagefile_usage as u64,
    )
}

#[cfg(not(windows))]
fn peaks() -> (u64, u64) {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| {
                rest.trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse::<u64>()
                    .ok()
            })
            .map_or(0, |kb| kb * 1024)
    };
    (kb("VmHWM:"), kb("VmPeak:"))
}

fn report(what: &str) {
    let (working_set, commit) = peaks();
    println!(
        "{what}: peak working set {:.2} MiB, peak commit {:.2} MiB",
        working_set as f64 / 1_048_576.0,
        commit as f64 / 1_048_576.0
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let state = args
        .get(1)
        .expect("usage: probe_peak <nse_state.bin> <ciphertexts.txt>");
    let lines = fs::read_to_string(args.get(2).expect("ciphertexts.txt")).unwrap();
    let ciphertexts: Vec<Vec<u8>> = lines.lines().map(|l| STANDARD.decode(l).unwrap()).collect();
    println!(
        "state file {} B, {} ciphertexts",
        fs::metadata(state).unwrap().len(),
        ciphertexts.len()
    );
    report("start");
    chat_nse::probe_decrypt(state.clone(), ciphertexts[0].clone()).unwrap();
    report("after 1 probe_decrypt");
    for ciphertext in &ciphertexts {
        chat_nse::probe_decrypt(state.clone(), ciphertext.clone()).unwrap();
    }
    report("after 9 in total");
}
