# iOS compatibility patches

These are same-version path patches for ChatProject's pinned iroh 1.3.0 graph.
They preserve the published files, except for the deltas below. `Cargo.toml.orig`
and `.cargo_vcs_info.json` remain untouched. Both crates are excluded from workspace
membership so ordinary workspace tests do not run upstream public-network tests.

| Crate | Published archive SHA-256 | Source revision / directory |
| --- | --- | --- |
| [iroh-dns 1.3.0](https://crates.io/api/v1/crates/iroh-dns/1.3.0/download) | `80276761b56e904e26ea71d0863beef0bb8d5ab6b639cbd1338e4b615209e3a1` | [17c0612f80f78f5288e97b818b1360ae6ea0a51a / iroh-dns](https://github.com/n0-computer/iroh/tree/17c0612f80f78f5288e97b818b1360ae6ea0a51a/iroh-dns) |
| [netwatch 0.19.3](https://crates.io/api/v1/crates/netwatch/0.19.3/download) | `39da9cad5f23aa43401f09497d3b280e51b5feba115d9ffbf38ca35d6ff95e96` | [0030e57fc70981895bb86212ff0f059cc41c8a4e / netwatch](https://github.com/n0-computer/net-tools/tree/0030e57fc70981895bb86212ff0f059cc41c8a4e/netwatch) |

The archives omit license texts. `LICENSE-MIT` and `LICENSE-APACHE` in each
directory are copied verbatim from that repository's root at the revision above.
Their SHA-256 values (MIT, Apache) are:

- iroh-dns: `f169adb8124d3b005416d8485d00777c9a7bdd9099982c52a4493f9732e6d050`,
  `903131e2786f073a942fbf8fae122d9e576e4dad758c6da7f9f2ba58fd8611ab`.
- netwatch: `33616ec4c88b6e00de66e6c06f1a55d80bb32a9d7939f3aa83d28e673a1adcae`,
  `7986218ec4ea89de3511a843ae27fea2584a525036c220ff7b26589179f07888`.

## Why this patch exists

The iOS static archive was force-loaded into `chat_ffi.framework`. Its native
framework metadata did not reach CocoaPods, and, independently, live upstream
DNS/interface code called macOS APIs that are private on iOS.
Apple's [SCDynamicStore header](https://github.com/apple-oss-distributions/configd/blob/main/SystemConfiguration.fproj/SCDynamicStore.h)
and [network-configuration header](https://github.com/apple-oss-distributions/configd/blob/main/SystemConfiguration.fproj/SCNetworkConfiguration.h)
mark these APIs public on macOS and SPI on iOS. Adding SystemConfiguration to the
linker would not make those calls public iOS APIs.

## Deltas from the archives

`iroh-dns/Cargo.toml`: exclude `n0-dns-resolver` on iOS; enable Tokio `net`/`rt`
for iOS and `net` for the host tests. Existing feature names are preserved.

`iroh-dns/src/dns.rs`: select the new private `dns/ios.rs` module on iOS (also
compile it for host tests), and cfg-gate the n0 adapter/conversions and its three
configuration tests to non-iOS. Document iOS builder semantics. Non-iOS
production behavior is unchanged.

`iroh-dns/src/dns/ios.rs`: implement the existing `Resolver` trait using
`tokio::net::lookup_host`, which delegates to the OS `getaddrinfo` with AF_UNSPEC;
filter the returned addresses into IPv4 or IPv6. The OS owns DNS routing,
including VPN/split DNS and DNS64. There is no application DNS cache, hardcoded
public fallback, constructor query, or reset I/O. Dropping a timed-out future
does not cancel an OS lookup already running on Tokio's blocking pool.

The supported builder requires system defaults and no explicit primary/fallback
nameservers. Deferred (default or explicit) and Never both mean OS-managed DNS
without application fallback. Eager, IfSystemEmpty, custom nameservers, a builder
without system defaults, and TXT queries return `DnsError::Resolve` with an
`io::ErrorKind::Unsupported` source. A supplied DNS TLS config is accepted but
has no role in OS DNS. This is scoped iOS support, not support for arbitrary DNS
configuration or a general TXT resolver.

M2 needs A/AAAA for `iroh-relay`'s TLS connection and iroh's network reports.
Its Minimal endpoint with Default relays installs Mainline DHT address lookup;
DHT signed-packet decoding uses `iroh_dns::pkarr`, not DNS TXT queries. It does
not install `DnsAddressLookup`. Mainline bootstrap already uses OS
`ToSocketAddrs` independently. Removing the whole n0 dependency on iOS is
necessary even with the adapter because the plugin force-loads its static archive.

`netwatch/Cargo.toml`: on iOS only, select the already-locked netdev 0.46.3 with
default features disabled and `gateway` enabled. Other targets retain their
published dependency declarations. This removes optional SystemConfiguration
extras while retaining public getifaddrs/Network interface discovery. No netwatch
Rust source changes are made. CocoaPods explicitly links the public Network
framework; no SystemConfiguration framework or private-symbol workaround is added.

## Standalone test lock and verification

`iroh-dns/Cargo.lock` is derived from the root lock at
`8025a7556375769309e12f66817fe87ffbefdea8` with the two path patches applied,
then pruned/resolved by Cargo 1.92.0. The published standalone lock selected older
versions, including libc 0.2.186 and Tokio 1.53.1. The derived lock preserves all
219 shared package name/version/checksum tuples, including libc 0.2.189 and Tokio
1.53.2; Cargo prunes 375 unused root entries and adds exactly eight entries:

- `n0-tracing-test` and `n0-tracing-test-macro` 0.3.0, the existing upstream test dependencies.
- `aws-lc-rs` 1.18.2, `aws-lc-sys` 0.46.0, `cmake` 0.1.58, `dunce` 1.0.5,
  `fs_extra` 1.3.0, and `jobserver` 0.1.35, resolved for the published optional
  `tls-aws-lc-rs` feature. They are not selected by the TLS-ring test or product graph.

Derived lock SHA-256 (LF-normalized):
`6cbb637add80db556c3db1794b85f31793dcf36b76bd2a584dc5aab8cbe44fb5`.
The root product lock only changes the two path-package source/checksum entries.
The netwatch standalone lock remains the original archive file and is unused.

Run the five local-only adapter tests with:

```sh
cargo +1.92.0 test --manifest-path vendor/iroh-dns/Cargo.toml --locked --lib dns::ios::tests --features tls-ring
```

The tests cover localhost/literals, family filtering, reset without runtime/I/O,
unsupported configuration/TXT, and OS error propagation. Host execution is not
proof of iOS DNS64/VPN behavior. Actual macOS CI must still build and link iOS
Simulator and unsigned Release, inspect final Mach-O imports, and run existing
integration tests. Signed/device and real DNS64/VPN validation remain separate.
