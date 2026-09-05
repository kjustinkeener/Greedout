//! Self-update: check GitHub for a newer build, verify it was signed by us, and
//! swap it in.
//!
//! Not `tauri-plugin-updater`. The app installs itself into one directory and
//! ships as a single exe, so an update is a download, a signature check and a
//! rename, and the plugin's machinery buys nothing that logic does not already
//! have. Four small crates instead.
//!
//! The security boundary is `verify_signature`, and it is the only thing
//! standing between a downloaded file and a program that runs on the user's
//! machine with their permissions. Nothing is written to disk before it passes.
//! The public half of the key is compiled in below; the private half exists on
//! the author's drive and in the release workflow's secrets, and never in this
//! repository.

use serde::{Deserialize, Serialize};
use std::io::Read;
use tauri::AppHandle;

/// GitHub resolves `latest` to the newest published, non-draft release, so a
/// draft release is invisible here. That is deliberate: a release is a draft
/// until its signature has been verified against this exact public key.
const MANIFEST_URL: &str =
    "https://github.com/kjustinkeener/Greedout/releases/latest/download/update.json";

/// Base64 of the whole minisign public key file, comment line included.
const PUBKEY_B64: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDFCNEUxQ0VBNzNDOTRBNTgKUldSWVNzbHo2aHhPRzkwbmMzWUxDbTh3RjN1dkNRUGlyZVYrdkJmNGpBcFpGZHh4RXdsTW81QWQK";

/// A hostile or broken manifest should not be able to make us read forever.
/// The real exe is around 13 MB.
const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    pub url: String,
    /// The full text of the `.minisig` file for `url`, decoded. Not base64 of
    /// it: the signing step emits base64, and the workflow decodes before
    /// writing the manifest.
    pub signature: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    pub current: String,
    pub available: Option<UpdateInfo>,
}

/// Ask whether a newer version exists. Never writes anything, never downloads
/// the exe: this is the call the launch path makes, so it is cheap and it fails
/// quietly.
#[tauri::command]
pub fn update_check() -> Result<CheckResult, String> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let body = http_get_string(MANIFEST_URL)?;
    let info: UpdateInfo =
        serde_json::from_str(&body).map_err(|e| format!("bad update manifest: {e}"))?;
    let cur =
        semver::Version::parse(&current).map_err(|e| format!("bad current version {current}: {e}"))?;
    let new = semver::Version::parse(info.version.trim_start_matches('v'))
        .map_err(|e| format!("bad manifest version {}: {e}", info.version))?;
    Ok(CheckResult {
        current,
        available: (new > cur).then_some(info),
    })
}

/// Download, verify, replace, relaunch. On success this never returns: the
/// replacement is already starting and this process exits.
#[tauri::command]
pub fn update_apply(app: AppHandle, info: UpdateInfo) -> Result<(), String> {
    let bytes = http_get_bytes(&info.url)?;
    // Order matters more here than anywhere else in the app: verify before the
    // bytes touch the disk, so a failed check leaves nothing behind to run.
    verify_signature(&bytes, &info.signature)?;
    self_replace_and_relaunch(&app, &bytes)
}

/// The whole security model. A download that fails this is discarded.
fn verify_signature(data: &[u8], sig_text: &str) -> Result<(), String> {
    use base64::Engine as _;
    let pubkey_file = base64::engine::general_purpose::STANDARD
        .decode(PUBKEY_B64)
        .map_err(|e| format!("pubkey decode: {e}"))?;
    let pubkey_file = String::from_utf8(pubkey_file).map_err(|e| format!("pubkey utf8: {e}"))?;
    // The key file is a comment line then the key line; take the key line.
    let key_line = pubkey_file
        .lines()
        .find(|l| !l.trim().is_empty() && !l.starts_with("untrusted comment:"))
        .ok_or("pubkey file has no key line")?;
    let pk = minisign_verify::PublicKey::from_base64(key_line.trim())
        .map_err(|e| format!("parse pubkey: {e}"))?;
    let sig =
        minisign_verify::Signature::decode(sig_text).map_err(|e| format!("parse signature: {e}"))?;
    // Legacy signatures allowed: the signer may emit either a prehashed or a
    // legacy minisign signature, and both come from the same trusted key.
    // Tighten this only after pinning which form the workflow actually emits.
    pk.verify(data, &sig, true)
        .map_err(|_| "signature verification FAILED - refusing to install".to_string())
}

/// Windows will not let a running exe be overwritten, but it will let it be
/// renamed. So: move ourselves aside, write the new build to our own path,
/// start it, and quit. Every failure below rolls the old exe back, because a
/// half-applied update leaves the user with no working app at all.
fn self_replace_and_relaunch(app: &AppHandle, new_bytes: &[u8]) -> Result<(), String> {
    let cur = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let old = cur.with_extension("old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&cur, &old).map_err(|e| format!("rename self aside: {e}"))?;
    if let Err(e) = std::fs::write(&cur, new_bytes) {
        let _ = std::fs::rename(&old, &cur);
        return Err(format!("write new exe: {e}"));
    }
    if let Err(e) = std::process::Command::new(&cur).spawn() {
        let _ = std::fs::remove_file(&cur);
        let _ = std::fs::rename(&old, &cur);
        return Err(format!("relaunch: {e}"));
    }
    // Drop the tray icon before we go, or the replacement's icon lands beside a
    // dead one that only disappears when the user hovers it.
    app.cleanup_before_exit();
    app.exit(0);
    Ok(())
}

/// Delete the copy the last update left behind. Best effort and retried every
/// launch, because the file can still be locked moments after the old process
/// exits. Called before the app builds anything.
pub fn cleanup_old() {
    if let Ok(cur) = std::env::current_exe() {
        let old = cur.with_extension("old");
        if old.exists() {
            let _ = std::fs::remove_file(&old);
        }
    }
}

fn http_get_bytes(url: &str) -> Result<Vec<u8>, String> {
    let resp = ureq::get(url)
        .set("User-Agent", "Greedout-Updater")
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(MAX_DOWNLOAD)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read body: {e}"))?;
    Ok(buf)
}

fn http_get_string(url: &str) -> Result<String, String> {
    String::from_utf8(http_get_bytes(url)?).map_err(|e| format!("response not utf8: {e}"))
}
