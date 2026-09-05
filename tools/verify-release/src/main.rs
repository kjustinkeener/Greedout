//! Check a drafted release the way the shipped app will, before anyone can
//! download it.
//!
//! A green build says the exe compiled, not that an installed copy will accept
//! it. Three things break independently of the build: the signing step, the
//! decode of the signature into the manifest, and drift between the key the
//! workflow signs with and the key compiled into the app. Each one produces a
//! perfect release that every existing install refuses.
//!
//! So this is a deliberate copy of the app's own verification, not a call into
//! it: same crate, same version, same public key, run against the real files
//! pulled back down from the release. If PUBKEY_B64 below ever drifts from
//! `src-tauri/src/update.rs`, this tool is worthless. Keep them identical.
//!
//! It also tampers with a byte and requires that the check then FAILS, because
//! a verifier that accepts everything also prints a cheerful OK, and that is
//! precisely the failure nobody notices.
//!
//! Usage: verify-release <dir containing greedout.exe and update.json>

const PUBKEY_B64: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDFCNEUxQ0VBNzNDOTRBNTgKUldSWVNzbHo2aHhPRzkwbmMzWUxDbTh3RjN1dkNRUGlyZVYrdkJmNGpBcFpGZHh4RXdsTW81QWQK";

fn verify(data: &[u8], sig_text: &str) -> Result<(), String> {
    use base64::Engine as _;
    let pubkey_file = base64::engine::general_purpose::STANDARD
        .decode(PUBKEY_B64)
        .map_err(|e| format!("pubkey decode: {e}"))?;
    let pubkey_file = String::from_utf8(pubkey_file).map_err(|e| format!("pubkey utf8: {e}"))?;
    let key_line = pubkey_file
        .lines()
        .find(|l| !l.trim().is_empty() && !l.starts_with("untrusted comment:"))
        .ok_or("pubkey file has no key line")?;
    let pk = minisign_verify::PublicKey::from_base64(key_line.trim())
        .map_err(|e| format!("parse pubkey: {e}"))?;
    let sig =
        minisign_verify::Signature::decode(sig_text).map_err(|e| format!("parse signature: {e}"))?;
    pk.verify(data, &sig, true).map_err(|e| format!("verify: {e}"))
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: verify-release <dir with greedout.exe and update.json>");
        std::process::exit(2);
    });
    let dir = std::path::Path::new(&dir);
    let exe = std::fs::read(dir.join("greedout.exe")).expect("read greedout.exe");
    let manifest = std::fs::read_to_string(dir.join("update.json")).expect("read update.json");
    let manifest: serde_json::Value = serde_json::from_str(&manifest).expect("parse update.json");
    let sig = manifest["signature"].as_str().expect("manifest has no signature");

    match verify(&exe, sig) {
        Ok(()) => println!("VERIFY_OK  version={}", manifest["version"]),
        Err(e) => {
            eprintln!("VERIFY_FAILED: {e}");
            std::process::exit(1);
        }
    }

    // The negative case. Without it, a verifier that says yes to anything looks
    // exactly like a verifier that works.
    let mut tampered = exe.clone();
    if let Some(b) = tampered.last_mut() {
        *b = b.wrapping_add(1);
    }
    let rejected = verify(&tampered, sig).is_err();
    println!("tampered rejected: {rejected}");
    if !rejected {
        eprintln!("VERIFIER IS BROKEN: it accepted a modified file");
        std::process::exit(1);
    }
}
