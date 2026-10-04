# Spike B: OpenMLS group state in an iOS NSE (desktop stand-in)

Date 2026-10-04. Windows 11 x86_64, rustc 1.92.0 (`x86_64-pc-windows-msvc`), `--release` with the default release profile.
Question (design 12.3, 14, 17 #1, 18): can the NSE load one group's MLS state and decrypt one message within about 15 MB?

## Bottom line

On the Rust heap, the MLS part is small. The worst measured case is rev3 config with N = 200. It needs 625 KiB of persisted state and a peak of 1.5 MiB to load and decrypt, taking about 3 ms.
For spike B's target group (50 people x 2 devices, so N = 100), rev3 needs 337 KiB of state and a 0.8 MiB peak.
These numbers assume tiny credentials. With InstallationCert-sized credentials (about 200 B), rev3 at N = 200 rises to 1.4 MiB of state and a 4.0 MiB peak (see the side experiments).
Even then, most of the 15 MB is likely to go to the Swift runtime, system frameworks, SQLite/SQLCipher, and dirty pages of the code. That has to be measured on a device.

## Results

`cargo run --release` runs each case in a fresh child process. Final run:

Release binary: **3,428,864 bytes** (`target/release/b-mls-memory.exe`, x86_64, not stripped, no LTO).

| N | scenario | (a) state KiB | (b) load+decrypt peak delta KiB | (c) ms, first / median of 5 | (d) process peak KiB |
|---|---|---|---|---|---|
| 10 | default | 18.1 | 19.6 | 0.18 / 0.10 | 380.0 |
| 50 | default | 56.4 | 56.7 | 0.30 / 0.22 | 845.2 |
| 100 | default | 103.4 | 99.7 | 0.44 / 0.36 | 1104.8 |
| 200 | default | 196.9 | 193.7 | 0.76 / 0.69 | 1771.2 |
| 10 | rev3 | 75.8 | 181.1 | 0.48 / 0.39 | 796.6 |
| 50 | rev3 | 192.7 | 560.9 | 1.08 / 0.92 | 1899.2 |
| 100 | rev3 | 336.6 | 801.2 | 1.71 / 1.53 | 3048.4 |
| 200 | rev3 | 625.3 | 1538.7 | 3.13 / 2.74 | 5605.9 |

- **(a) state**: the sum of key and value bytes in R's `MemoryStorage` map.
- **(b) peak delta**: the peak heap above the baseline. It covers `MlsGroup::load`, TLS-decoding the 1,170-byte ciphertext, and `process_message` of a 1 KiB message. The work runs on a fresh provider whose storage is a copy of R's map. The copy is already in the baseline.
- **(c) time**: wall time for (b). "First" is the first of 5 repetitions; each repetition uses a fresh copy.
- **(d) process peak**: the peak of the whole child process so far, including setup with S, R, all KeyPackages, commits, and 201 messages. It describes this benchmark, not the NSE.
- **Run-to-run variation** (5 full runs): (a) and (b) agree within 1 KiB, with two exceptions. (b) for 50/default ranged 52.6–56.7 KiB, and for 200/default 185.6–193.7 KiB. The cause is a JSON blob landing near a power of two, so `Vec` capacity doubling flips. Times vary by about ±25%.

**Conservative NSE estimate for the Rust heap.** A SQLite-backed NSE does not hold the whole map in memory; it reads blobs on demand. So (a) + (b) is an upper bound:

| | rev3 N = 100 | rev3 N = 200 |
|---|---|---|
| tiny credentials | 1.1 MiB | 2.1 MiB (~15% of 15 MB) |
| 200 B credentials | 2.7 MiB | 5.4 MiB (~38% of 15 MB) |

### What the state is made of (N = 200, KiB by storage label)

| label | default | rev3 |
|---|---|---|
| MessageSecrets | 8.3 | 436.7 |
| Tree | 182.8 | 182.8 |
| EpochKeyPairs | 2.6 | 2.7 |
| everything else (9 labels) | 3.1 | 3.1 |

The program prints the full breakdown for every case. A temporary dump of the rev3, N = 200 `MessageSecrets` blob (448,111 B) shows its contents:

- 5 past-epoch entries total 410,790 B. Each entry holds a **full copy of the member list** (`EpochTree.leaves: Vec<Member>`: credential, encryption key, signature key) at about 74 KB, plus about 7.7 KB of secret tree.
- Those 5 member lists are 83% of `MessageSecrets` and about 58% of the whole state.
- The current epoch is 37 KB. Of that, about 31 KB is the 199 retained application key/nonce pairs from S's skipped messages (`out_of_order_tolerance = 200`).

### Where the rev3 peak comes from (N = 200, temporary instrumented run)

- `MlsGroup::load`: 597 KiB peak. The live `MlsGroup` takes 589 KiB, because all 5 past epochs are deserialized even though the message is from the current epoch.
- `process_message`: a further **951 KiB**. Nearly all of it comes from re-serializing the whole `MessageSecretsStore` to JSON, which happens after every decrypted `PrivateMessage` (`write_message_secrets`). That means a `serde_json::to_vec` buffer grown to 512 KiB, plus the `value.to_vec()` copy (437 KiB) in `MemoryStorage::write`. The decryption itself is negligible.
- The default config by comparison: load 177 KiB, decrypt +26 KiB.

## Versions (from Cargo.lock)

| crate | version |
|---|---|
| openmls | 0.9.0 (latest 0.9.x on crates.io, default features) |
| openmls_rust_crypto | 0.6.0 |
| openmls_basic_credential | 0.6.0 |
| openmls_memory_storage | 0.6.0 (the storage inside `OpenMlsRustCrypto`; values are serde_json) |
| openmls_traits | 0.6.0 |
| tls_codec | 0.5.0 |
| hpke-rs / hpke-rs-rust-crypto | 0.7.0 / 0.7.0 |
| serde_json | 1.0.151 |
| ed25519-dalek, x25519-dalek, aes-gcm | 2.2.0, 2.0.1, 0.10.3 (x25519-dalek 3.0.0 is also linked, through hpke-rs-rust-crypto's `x-wing` PQ KEM; this ciphersuite does not use it) |

The ciphersuite is `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (design 6.4). The API was checked against the openmls 0.9.0 source and the OpenMLS book.

## Method

1. **Members.** S and R each get their own `OpenMlsRustCrypto` provider, an Ed25519 `SignatureKeyPair` (stored in the provider), and a `BasicCredential`. The other N-2 members each get a throwaway provider that only makes a KeyPackage and is then dropped; those members never process anything. N counts S and R.
2. **Group build.** S creates the group and adds members by value, up to 50 per commit: 1, 1, 2, 4 add commits for N = 10, 50, 100, 200. R's KeyPackage is in the first batch. R joins from the Welcome; the default config has no ratchet-tree extension, so the tree is passed out of band. R then processes the remaining add commits. Every message crosses the TLS wire encoding.
3. **Scenarios** (R's join config matches the creator config):
   - `default`: `MlsGroupCreateConfig::default()`, so `max_past_epochs = 0` and the sender ratchet is (5, 1000).
   - `rev3`: `.max_past_epochs(5)` and `.sender_ratchet_configuration(SenderRatchetConfiguration::new(200, 1000))`.
4. **Traffic, identical in both scenarios** so that only the config differs:
   - 6 self-update commits by S, which R processes.
   - S sends 200 messages of 1 KiB; R decrypts only the last (generation 199).
   - The default config retains almost nothing from this traffic. A no-traffic check at default N = 200 gave 194.8 KiB state and a 185.0 KiB peak, the same within run-to-run variation.
5. **Measurement.**
   - S encrypts one more 1 KiB message.
   - (a) R's storage map is cloned and its key and value bytes are summed.
   - (b)(c) The clone goes into a fresh `OpenMlsRustCrypto`. The peak is reset to the current heap, then load and decrypt run under a timer. Writes land in the copy and are discarded, the read-only overlay of design 12.3.
   - This repeats 5 times on fresh copies.
   - A counting `#[global_allocator]` wraps `System` and tracks current and peak requested bytes. `realloc` is counted conservatively (old + new block at once); counting it in place gave identical peaks.
6. Each (N, scenario) runs in its own process, so (d) and the allocator state do not leak between cases.

## Caveats

- **Windows heap is not iOS `phys_footprint`.** The measurement counts bytes Rust requested. iOS jetsam charges dirty and compressed memory: malloc regions including fragmentation and freed-but-retained pages, stacks, and dirty data pages of every loaded image. Rounding every allocation up to 16 B added only 0.3–1.7%, because at most about 4.8k live allocations exist at the peak. Fragmentation and region retention are not modeled.
- **The NSE also carries the Swift runtime, system frameworks, and code pages.** Clean, file-backed text pages are generally not charged to the footprint, but relocated and dirty data pages are. The x86_64 binary size above is only a proxy for arm64. An empty Swift NSE's baseline must be measured on a device (Xcode memory gauge or `TASK_VM_INFO.phys_footprint`).
- **SQLite/SQLCipher is not included.** The default page cache is up to about 2 MB (`cache_size = -2000`), already larger than the MLS heap here. Set it low in the NSE.
- **Storage is serde_json.** Byte strings are stored as decimal arrays (`{"vec":[222,109,...]}`), about 6–7x the raw bytes. A SQLite storage with a serde_json codec would hold blobs of about the same size. A binary codec would shrink both (a) and (b) substantially (not measured).
- **The per-past-epoch member list scales the rev3 state.** It grows as O(N x (1 + max_past_epochs)), and credential size multiplies it: each credential is stored 6 times, once in the tree and once in each of 5 past epochs. The cheapest knobs are a smaller `max_past_epochs` and a compact credential.
- **Write amplification in the main app (not the NSE).** OpenMLS 0.9 rewrites the whole `MessageSecretsStore` blob on every message sent (`MlsGroup::encrypt`) and every `PrivateMessage` received (`unprotect_message`). In rev3 at N = 200 that is about 437 KiB per message, or about 1.1 MiB with 200 B credentials, which a SQLite storage would rewrite every time.
- **Possible NSE optimization (read in code, not tested).** The NSE's StorageProvider could ignore `write_message_secrets` instead of buffering it, so the blob would never be serialized. That would skip the re-serialization transient, about 60% of the rev3 peak. In the 0.9.0 code with default features, an application message's only storage access after `load` is that write (`unprotect_message`). After it, parsing, signature verification, and the result use in-memory state only.
- **This is a best case for group shape and traffic:**
  - Only S commits, so only S's direct path has non-blank parent nodes. A real group where many members commit has up to N-1 parent nodes, so the tree gets bigger (not measured).
  - Only one sender is exercised. With `out_of_order_tolerance = 200`, every sender R has skipped messages from can retain up to 200 key/nonce pairs (about 31 KB JSON) per epoch, in the current epoch and in each of the 5 retained past epochs.
  - Credentials are 1–4 B `BasicCredential`s; the design puts an InstallationCert in them (design 4.1).
- **Timing comes from a warm process on a desktop CPU.** "First" is the closest to a cold NSE start. All results are far under the NSE's roughly 30 s, but they do not validate the "< 1 ms" row of design 14 for rev3: load + decrypt is 0.4–2.7 ms (median). The time tracks state size, so JSON (de)serialization probably dominates; the two were not timed separately.

## Side experiments (temporary code changes, reverted)

Credentials sized like an InstallationCert. Every member's `BasicCredential` identity was 200 pseudo-random bytes. To reproduce, replace `name.as_bytes().to_vec()` with `signer.public().iter().cycle().take(200).copied().collect()` in `Client::new`.

| N | scenario | (a) state KiB | (b) peak delta KiB | ms median | (d) process peak KiB |
|---|---|---|---|---|---|
| 10 | default | 25.2 | 22.1 | 0.10 | 412.2 |
| 100 | default | 172.4 | 123.9 | 0.53 | 1412.0 |
| 200 | default | 334.8 | 242.1 | 1.03 | 2525.0 |
| 10 | rev3 | 117.5 | 294.2 | 0.59 | 1103.6 |
| 100 | rev3 | 749.1 | 2058.2 | 2.97 | 6224.8 |
| 200 | rev3 | 1451.4 | 4053.6 | 5.95 | 11963.0 |

Other checks:

- **rev3 without the traffic phase** (only the add commits, N = 200): 320.9 KiB state, 698.6 KiB peak. After 3 add commits, R holds 3 past epochs with smaller member lists.
- **Decomposing (b) for rev3 at N = 10**: load 59 KiB, decrypt +126 KiB. At small N the 199 retained ratchet secrets dominate, not the member lists.
