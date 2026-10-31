//! Usage: nse_fixture <out_dir> [k = 8] [credential_bytes = 200]
//! Writes nse_state.bin (N = 200) and ciphertexts.txt (one base64 per line, line i = seq i - 1).

use std::{fs, path::PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = PathBuf::from(args.get(1).expect("usage: nse_fixture <out_dir> [k] [credential_bytes]"));
    let k: usize = args.get(2).map_or(8, |a| a.parse().unwrap());
    let credential_bytes: usize = args.get(3).map_or(200, |a| a.parse().unwrap());

    let fixture = nse_fixture::build(200, k, credential_bytes);
    fs::create_dir_all(&out).unwrap();
    let state_path = out.join("nse_state.bin");
    fs::write(&state_path, &fixture.state).unwrap();

    // Self-check through the same entry point the NSE calls.
    for (seq, ct) in fixture.ciphertexts.iter().enumerate() {
        let outcome = chat_nse::probe_decrypt(state_path.to_string_lossy().into_owned(), ct.clone()).unwrap();
        assert_eq!(outcome.plaintext_len, 1024);
        println!(
            "seq {seq}: ciphertext {} B, base64 {} B, load {} us, decrypt {} us",
            ct.len(),
            STANDARD.encode(ct).len(),
            outcome.load_micros,
            outcome.decrypt_micros,
        );
    }
    let lines: Vec<String> = fixture.ciphertexts.iter().map(|ct| STANDARD.encode(ct)).collect();
    fs::write(out.join("ciphertexts.txt"), lines.join("\n") + "\n").unwrap();
    println!("state {} B", fixture.state.len());
}
