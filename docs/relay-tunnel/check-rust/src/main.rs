use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use chacha20poly1305::{aead::{Aead, KeyInit, Payload}, ChaCha20Poly1305, Nonce};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn h(v: &Value) -> Vec<u8> { hex::decode(v.as_str().unwrap()).unwrap() }
fn b(v: &Value) -> Vec<u8> { B64.decode(v.as_str().unwrap()).unwrap() }
fn hk(ikm: &[u8], info: &str, n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n];
    hkdf::Hkdf::<Sha256>::new(Some(b"hive-tunnel/1"), ikm).expand(info.as_bytes(), &mut out).unwrap();
    out
}
fn ctx(label: &str, parts: &[&[u8]]) -> Vec<u8> {
    let mut o = format!("hive-tunnel/1 {label}").into_bytes();
    o.push(0);
    for p in parts { o.extend_from_slice(p); }
    o
}

fn check_transcript(t: &Value, keys: &Value) {
    let proto = t["protocol"].as_str().unwrap();
    let prologue = h(&t["prologueHex"]);
    let host_pub = b(&keys["hostStaticPub"]);
    let (pk_i, pk_r) = (h(&keys["phoneStaticPrivHex"]), h(&keys["hostStaticPrivHex"]));
    let (e_i, e_r) = (h(&t["phoneEphemeralPrivHex"]), h(&t["hostEphemeralPrivHex"]));
    let mut ib = snow::Builder::new(proto.parse().unwrap())
        .local_private_key(&pk_i)
        .remote_public_key(&host_pub)
        .prologue(&prologue)
        .fixed_ephemeral_key_for_testing_only(&e_i);
    let mut rb = snow::Builder::new(proto.parse().unwrap())
        .local_private_key(&pk_r)
        .prologue(&prologue)
        .fixed_ephemeral_key_for_testing_only(&e_r);
    let psk;
    if t.get("pskHex").is_some() {
        psk = h(&t["pskHex"]);
        ib = ib.psk(1, &psk);
        rb = rb.psk(1, &psk);
    }
    let mut ini = ib.build_initiator().unwrap();
    let mut res = rb.build_responder().unwrap();
    let mut buf = vec![0u8; 65535];
    let mut out = vec![0u8; 65535];
    let n = ini.write_message(t["msg1Payload"].as_str().unwrap().as_bytes(), &mut buf).unwrap();
    assert_eq!(hex::encode(&buf[..n]), t["msg1Hex"].as_str().unwrap(), "{proto} msg1");
    let m = res.read_message(&buf[..n], &mut out).unwrap();
    assert_eq!(&out[..m], t["msg1Payload"].as_str().unwrap().as_bytes());
    assert_eq!(res.get_remote_static().unwrap(), &B64.decode(keys["phoneStaticPub"].as_str().unwrap()).unwrap()[..]);
    let n = res.write_message(t["msg2Payload"].as_str().unwrap().as_bytes(), &mut buf).unwrap();
    assert_eq!(hex::encode(&buf[..n]), t["msg2Hex"].as_str().unwrap(), "{proto} msg2");
    let m = ini.read_message(&buf[..n], &mut out).unwrap();
    assert_eq!(&out[..m], t["msg2Payload"].as_str().unwrap().as_bytes());
    let hh = ini.get_handshake_hash().to_vec();
    assert_eq!(hex::encode(&hh), t["handshakeHashHex"].as_str().unwrap());
    if let Some(pk) = t.get("pushKeyHex") {
        assert_eq!(hex::encode(hk(&hh, "push-key", 32)), pk.as_str().unwrap());
    }
    let mut ini = ini.into_transport_mode().unwrap();
    let mut res = res.into_transport_mode().unwrap();
    for msg in t["transport"].as_array().unwrap() {
        let pt = h(&msg["plaintextHex"]);
        let n = if msg["dir"] == "phone->host" { ini.write_message(&pt, &mut buf).unwrap() } else { res.write_message(&pt, &mut buf).unwrap() };
        assert_eq!(hex::encode(&buf[..n]), msg["wireHex"].as_str().unwrap());
        let m = if msg["dir"] == "phone->host" { res.read_message(&buf[..n], &mut out).unwrap() } else { ini.read_message(&buf[..n], &mut out).unwrap() };
        assert_eq!(&out[..m], &pt[..]);
    }
    println!("ok: {proto} ({} transport messages)", t["transport"].as_array().unwrap().len());
}

fn main() {
    let v: Value = serde_json::from_slice(&std::fs::read(std::env::args().nth(1).unwrap()).unwrap()).unwrap();
    let keys = &v["keys"];
    let host_sign = SigningKey::from_bytes(&h(&keys["hostSignSeedHex"]).try_into().unwrap());
    let phone_auth = SigningKey::from_bytes(&h(&keys["phoneAuthSeedHex"]).try_into().unwrap());
    assert_eq!(B64.encode(host_sign.verifying_key().as_bytes()), keys["hostSignPub"].as_str().unwrap());
    assert_eq!(B64.encode(phone_auth.verifying_key().as_bytes()), keys["phoneAuthPub"].as_str().unwrap());
    for (p, q) in [("hostStaticPrivHex", "hostStaticPub"), ("phoneStaticPrivHex", "phoneStaticPub")] {
        let s = x25519_dalek::StaticSecret::from(<[u8; 32]>::try_from(h(&keys[p])).unwrap());
        assert_eq!(B64.encode(x25519_dalek::PublicKey::from(&s).as_bytes()), keys[q].as_str().unwrap());
    }
    let host_id = B64.encode(&Sha256::digest(ctx("host-id", &[host_sign.verifying_key().as_bytes()]))[..16]);
    assert_eq!(host_id, v["hostId"].as_str().unwrap());
    let c = &v["challenge"];
    let nonce = b(&c["nonce"]);
    let ha = ctx("host-auth", &[host_id.as_bytes(), &nonce]);
    let pa = ctx("phone-auth", &[host_id.as_bytes(), &nonce]);
    assert_eq!(hex::encode(&ha), c["hostAuthInputHex"].as_str().unwrap());
    assert_eq!(hex::encode(&pa), c["phoneAuthInputHex"].as_str().unwrap());
    assert_eq!(B64.encode(host_sign.sign(&ha).to_bytes()), c["hostAuthSig"].as_str().unwrap());
    assert_eq!(B64.encode(phone_auth.sign(&pa).to_bytes()), c["phoneAuthSig"].as_str().unwrap());
    let claims = v["credential"]["claimsJson"].as_str().unwrap();
    let signed = format!("hpc1.{}", B64.encode(claims));
    let tok = format!("{signed}.{}", B64.encode(host_sign.sign(signed.as_bytes()).to_bytes()));
    assert_eq!(tok, v["credential"]["token"].as_str().unwrap());
    let p = &v["pairing"];
    let ps = b(&p["pairingSecret"]);
    assert_eq!(B64.encode(hk(&ps, "enroll-id", 16)), p["eid"].as_str().unwrap());
    let tag = hk(&ps, "relay-tag", 32);
    assert_eq!(B64.encode(&tag), p["tag"].as_str().unwrap());
    assert_eq!(B64.encode(Sha256::digest(&tag)), p["tagHash"].as_str().unwrap());
    assert_eq!(hex::encode(hk(&ps, "noise-psk", 32)), p["pskHex"].as_str().unwrap());
    assert_eq!(hex::encode(ctx("noise", &[host_id.as_bytes()])), v["session"]["prologueHex"].as_str().unwrap());
    check_transcript(&v["enrollment"], keys);
    check_transcript(&v["session"], keys);
    let push = &v["push"];
    let key = h(&v["enrollment"]["pushKeyHex"]);
    let ct = b(&push["ct"]);
    let ad = ctx("push", &[host_id.as_bytes(), v["pid"].as_str().unwrap().as_bytes()]);
    assert_eq!(hex::encode(&ad), push["adHex"].as_str().unwrap());
    let pt = ChaCha20Poly1305::new_from_slice(&key).unwrap()
        .decrypt(Nonce::from_slice(&ct[..12]), Payload { msg: &ct[12..], aad: &ad }).unwrap();
    assert_eq!(pt, push["plaintext"].as_str().unwrap().as_bytes());
    println!("ok: host id, challenge sigs, credential, pairing derivations, push");
    // Negative vectors.
    let neg = &v["negative"];
    let t = neg["credentialTamperedClaims"].as_str().unwrap();
    let (signed, sig) = t.rsplit_once('.').unwrap();
    let sig = ed25519_dalek::Signature::from_slice(&B64.decode(sig).unwrap()).unwrap();
    assert!(host_sign.verifying_key().verify_strict(signed.as_bytes(), &sig).is_err());
    let keys = &v["keys"];
    let s = &v["session"];
    let mk_resp = || {
        let k = h(&keys["hostStaticPrivHex"]);
        snow::Builder::new(s["protocol"].as_str().unwrap().parse().unwrap()).local_private_key(&k).prologue(&h(&s["prologueHex"])).build_responder().unwrap()
    };
    let mut out = vec![0u8; 65535];
    assert!(mk_resp().read_message(&h(&neg["sessionMsg1OtherHostPrologueHex"]), &mut out).is_err(), "wrong prologue must fail");
    // Tampered msg2: initiator with the vector's ephemeral reads it.
    let (pk_i, e_i) = (h(&keys["phoneStaticPrivHex"]), h(&s["phoneEphemeralPrivHex"]));
    let hp = b(&keys["hostStaticPub"]);
    let mut ini = snow::Builder::new(s["protocol"].as_str().unwrap().parse().unwrap()).local_private_key(&pk_i).remote_public_key(&hp)
        .prologue(&h(&s["prologueHex"])).fixed_ephemeral_key_for_testing_only(&e_i).build_initiator().unwrap();
    let mut buf = vec![0u8; 65535];
    ini.write_message(b"{\"v\":1}", &mut buf).unwrap();
    assert!(ini.read_message(&h(&neg["sessionMsg2TamperedHex"]), &mut out).is_err(), "tampered msg2 must fail");
    println!("ok: negative vectors rejected");
}
