use chat_core::account::{
    DeviceKind, Generation, IdentityRecord, InstallationCert, RecordDevice, Secret32,
    derive_account, derive_device, parse_record, record_hash, record_packet, sign_cert,
    verify_cert,
};
use data_encoding::BASE64URL_NOPAD;
use pkarr::{Keypair, SignedPacket, Timestamp, dns::rdata::TXT};

const NOW: u64 = 1_800_000_000;
const IDENTITY_SEED: [u8; 32] = [
    0x3c, 0x5a, 0x13, 0xc8, 0x79, 0xac, 0x32, 0x55, 0xf7, 0xe9, 0x25, 0x97, 0x72, 0xc0, 0xf6, 0xf0,
    0xe8, 0xd8, 0xca, 0xc2, 0x55, 0xd8, 0x1a, 0x9e, 0xe8, 0x10, 0x78, 0xda, 0x74, 0xfd, 0x6a, 0x01,
];

fn record(count: u8) -> IdentityRecord {
    let mut devices: Vec<_> = (0..count)
        .map(|i| {
            let d = derive_device(Secret32::from([i; 32])).unwrap();
            RecordDevice {
                installation_id: d.installation_id(),
                transport_public_key: d.transport_public_key(),
                kind: DeviceKind::Desktop,
            }
        })
        .collect();
    devices.sort_by_key(|d| d.installation_id);
    IdentityRecord {
        generation: Generation([7; 16]),
        issued_at: NOW,
        expires_at: NOW + 604800,
        devices,
    }
}

fn packet_payload(payload: &[u8]) -> SignedPacket {
    let text = BASE64URL_NOPAD.encode(payload);
    SignedPacket::builder()
        .txt(
            "_account".try_into().unwrap(),
            TXT::try_from(text.as_str()).unwrap(),
            86400,
        )
        .timestamp(Timestamp::from(NOW * 1_000_000))
        .sign(&Keypair::from_secret_key(&IDENTITY_SEED))
        .unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks_exact(2)
        .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn golden_cert_record_dns_wrapper() {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/m3-wire-vectors.json")).unwrap();
    let keys = derive_account(&Secret32::from([0; 32])).unwrap();
    let device = derive_device(Secret32::from([0; 32])).unwrap();
    let cert = sign_cert(&keys, &device, DeviceKind::Desktop, NOW).unwrap();
    assert_eq!(hex(&cert.to_bytes()), v["cert_hex"]);
    assert_eq!(
        InstallationCert::from_bytes(&unhex(v["cert_hex"].as_str().unwrap())).unwrap(),
        cert
    );
    verify_cert(&cert, keys.peer_id()).unwrap();
    let r = record(5);
    assert_eq!(hex(&r.to_bytes().unwrap()), v["record_hex"]);
    let p = record_packet(&keys, &r, NOW * 1_000_000).unwrap();
    assert_eq!(hex(&p.encoded_packet()), v["dns_hex"]);
    assert_eq!(hex(p.as_bytes()), v["wrapper_hex"]);
    assert_eq!(hex(&cert.hash()), v["cert_sha256"]);
    assert_eq!(hex(&record_hash(&p)), v["packet_sha256"]);
}

#[test]
fn five_devices_under_1000_wrapper_and_bep44() {
    let keys = derive_account(&Secret32::from([0; 32])).unwrap();
    let r = record(5);
    let p = record_packet(&keys, &r, NOW * 1_000_000).unwrap();
    assert!(p.as_bytes().len() <= 1000);
    assert!(p.encoded_packet().len() <= 1000);
    assert_eq!(parse_record(&p, keys.peer_id(), NOW).unwrap(), r);
    assert_eq!(p.all_resource_records().count(), 1);
    // One RR contains several DNS character strings; no cross-RR concatenation.
    let dns = p.encoded_packet();
    let mut offset = 12;
    while dns[offset] != 0 {
        offset += 1 + dns[offset] as usize;
    }
    offset += 11;
    let mut chunks = 0;
    while offset < dns.len() {
        let n = dns[offset] as usize;
        assert!(n <= 255);
        offset += n + 1;
        chunks += 1;
    }
    assert_eq!(offset, dns.len());
    assert!(chunks > 1);
}

#[test]
fn six_devices_and_duplicate_rejected() {
    let keys = derive_account(&Secret32::from([0; 32])).unwrap();
    assert!(record_packet(&keys, &record(6), 1).is_err());
    let mut r = record(2);
    r.devices[1] = r.devices[0].clone();
    assert!(record_packet(&keys, &r, 1).is_err());
    let mut r = record(2);
    r.devices[1].transport_public_key = r.devices[0].transport_public_key;
    assert!(record_packet(&keys, &r, 1).is_err());
    let mut r = record(2);
    r.devices.reverse();
    assert!(record_packet(&keys, &r, 1).is_err());
}

#[test]
fn strict_cbor_and_wrong_domain_rejected() {
    let keys = derive_account(&Secret32::from([0; 32])).unwrap();
    let d = derive_device(Secret32::from([0; 32])).unwrap();
    let cert = sign_cert(&keys, &d, DeviceKind::Desktop, NOW).unwrap();
    let bytes = cert.to_bytes();
    let mut nonminimal = bytes.clone();
    nonminimal.splice(1..2, [0x18, 1]);
    let mut trailing = bytes.clone();
    trailing.push(0);
    let mut indefinite = bytes.clone();
    indefinite[0] = 0x9f;
    indefinite.push(0xff);
    for bad in [
        nonminimal,
        trailing,
        indefinite,
        vec![0xa0],
        vec![0xc0, 0x80],
    ] {
        assert!(InstallationCert::from_bytes(&bad).is_err());
    }
    let mut wrong = cert.clone();
    let mut body = bytes[..bytes.len() - 66].to_vec();
    body[0] = 0x86;
    let mut message = b"ChatProject/identity-invite/v1\x01".to_vec();
    message.extend(body);
    wrong.sig = Keypair::from_secret_key(&IDENTITY_SEED)
        .sign(&message)
        .to_bytes();
    assert!(verify_cert(&wrong, keys.peer_id()).is_err());
    let mut tampered = cert;
    tampered.transport_public_key[0] ^= 1;
    assert!(verify_cert(&tampered, keys.peer_id()).is_err());
    let other = derive_account(&Secret32::from([1; 32])).unwrap();
    assert!(verify_cert(&tampered, other.peer_id()).is_err());
    let payload = record(1).to_bytes().unwrap();
    let mut bad = payload.clone();
    bad.splice(1..2, [0x18, 1]);
    assert!(parse_record(&packet_payload(&bad), keys.peer_id(), NOW).is_err());
    let mut bad = payload;
    bad.push(0);
    assert!(parse_record(&packet_payload(&bad), keys.peer_id(), NOW).is_err());
}

#[test]
fn packet_has_no_ip_cert_name_relay() {
    let keys = derive_account(&Secret32::from([0; 32])).unwrap();
    let p = record_packet(&keys, &record(5), 1).unwrap();
    let rr = p.all_resource_records().next().unwrap();
    assert_eq!(
        rr.name.to_string(),
        format!("_account.{}", p.public_key().to_z32())
    );
    assert_ne!(p.public_key().to_z32(), keys.peer_id().to_string());
    assert_eq!(rr.ttl, 86400);
    assert!(matches!(rr.rdata, pkarr::dns::rdata::RData::TXT(_)));
    let r = parse_record(&p, keys.peer_id(), NOW).unwrap();
    assert_eq!(r, record(5)); // Exact typed payload contains only the approved fields.
    let kp = Keypair::from_secret_key(&IDENTITY_SEED);
    let text = BASE64URL_NOPAD.encode(&record(1).to_bytes().unwrap());
    for p in [
        SignedPacket::builder()
            .txt(
                "_account".try_into().unwrap(),
                text.as_str().try_into().unwrap(),
                86400,
            )
            .address("x".try_into().unwrap(), "127.0.0.1".parse().unwrap(), 86400)
            .sign(&kp)
            .unwrap(),
        SignedPacket::builder()
            .txt(
                "_account".try_into().unwrap(),
                text.as_str().try_into().unwrap(),
                86400,
            )
            .txt(
                "_account".try_into().unwrap(),
                text.as_str().try_into().unwrap(),
                86400,
            )
            .sign(&kp)
            .unwrap(),
        SignedPacket::builder()
            .txt(
                "wrong".try_into().unwrap(),
                text.as_str().try_into().unwrap(),
                86400,
            )
            .sign(&kp)
            .unwrap(),
    ] {
        assert!(parse_record(&p, keys.peer_id(), NOW).is_err());
    }
}

#[test]
fn application_expiry_is_independent_of_dns_cache_and_seq() {
    let keys = derive_account(&Secret32::from([0; 32])).unwrap();
    let r = record(1);
    let mut p = record_packet(&keys, &r, 1).unwrap();
    p.set_last_seen(&Timestamp::from(0));
    assert!(p.is_expired(0, 86400));
    let parsed = parse_record(&p, keys.peer_id(), NOW).unwrap();
    assert!(!parsed.is_expired(NOW));
    let parsed = parse_record(&p, keys.peer_id(), NOW + 604800).unwrap();
    assert!(parsed.is_expired(NOW + 604800));
    assert!(parse_record(&p, keys.peer_id(), NOW - 301).is_err());
    assert!(parse_record(&p, keys.peer_id(), NOW - 300).is_ok());
    let mut bad = r.clone();
    bad.expires_at += 1;
    assert!(record_packet(&keys, &bad, 1).is_err());
    bad.expires_at = bad.issued_at;
    assert!(record_packet(&keys, &bad, 1).is_err());
    let other = derive_account(&Secret32::from([1; 32])).unwrap();
    assert!(parse_record(&p, other.peer_id(), NOW).is_err());
    assert!(record_packet(&keys, &r, i64::MAX as u64 + 1).is_err());
}

#[test]
fn rejects_unverified_oversized_and_unapproved_dns_sections() {
    let keys = derive_account(&Secret32::from([0; 32])).unwrap();
    let p = record_packet(&keys, &record(5), 1).unwrap();
    let mut serialized = p.serialize();
    serialized[8 + 32] ^= 1; // pkarr deserialize intentionally does not verify signatures.
    let unchecked = SignedPacket::deserialize(&serialized).unwrap();
    assert!(parse_record(&unchecked, keys.peer_id(), NOW).is_err());
    let kp = Keypair::from_secret_key(&IDENTITY_SEED);
    let text = BASE64URL_NOPAD.encode(&record(5).to_bytes().unwrap());
    let mut txt = TXT::try_from(text.as_str()).unwrap();
    for _ in 0..310 {
        txt.add_string("").unwrap();
    }
    let oversized = SignedPacket::builder()
        .txt("_account".try_into().unwrap(), txt, 86400)
        .sign(&kp)
        .unwrap();
    assert!(oversized.encoded_packet().len() <= 1000);
    assert!(oversized.as_bytes().len() > 1000);
    assert!(parse_record(&oversized, keys.peer_id(), NOW).is_err());
    let dns_bytes = p.encoded_packet();
    let mut dns = pkarr::dns::Packet::parse(&dns_bytes).unwrap();
    dns.additional_records.push(dns.answers[0].clone());
    let encoded = dns.build_bytes_vec_compressed().unwrap();
    assert!(parse_record(&signed_dns(&encoded), keys.peer_id(), NOW).is_err());
    let mut trailing = p.encoded_packet().to_vec();
    trailing.push(0);
    assert!(parse_record(&signed_dns(&trailing), keys.peer_id(), NOW).is_err());
}

fn signed_dns(dns: &[u8]) -> SignedPacket {
    let mut message = format!("3:seqi1e1:v{}:", dns.len()).into_bytes();
    message.extend(dns);
    let kp = Keypair::from_secret_key(&IDENTITY_SEED);
    let mut serialized = vec![0; 8];
    serialized.extend(kp.public_key().as_bytes());
    serialized.extend(kp.sign(&message).to_bytes());
    serialized.extend(1u64.to_be_bytes());
    serialized.extend(dns);
    SignedPacket::deserialize(&serialized).unwrap()
}
