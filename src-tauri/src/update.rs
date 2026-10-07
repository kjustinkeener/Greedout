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
#[cfg(windows)]
use std::io::Read;
use std::path::Path;
#[cfg(any(windows, test))]
use std::path::PathBuf;
use tauri::AppHandle;

/// GitHub resolves `latest` to the newest published, non-draft release, so a
/// draft release is invisible here. That is deliberate: a release is a draft
/// until its signature has been verified against this exact public key.
#[cfg(windows)]
const MANIFEST_URL: &str =
    "https://github.com/kjustinkeener/Greedout/releases/latest/download/update.json";

/// Base64 of the whole minisign public key file, comment line included.
#[cfg(windows)]
const PUBKEY_B64: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDFCNEUxQ0VBNzNDOTRBNTgKUldSWVNzbHo2aHhPRzkwbmMzWUxDbTh3RjN1dkNRUGlyZVYrdkJmNGpBcFpGZHh4RXdsTW81QWQK";

/// A hostile or broken manifest should not be able to make us read forever.
/// The real exe is around 13 MB.
#[cfg(windows)]
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
    #[cfg(not(windows))]
    return Err(
        "Updates on Linux are delivered by the installed package or AppImage download".to_string(),
    );

    #[cfg(windows)]
    {
        let current = env!("CARGO_PKG_VERSION").to_string();
        let body = http_get_string(MANIFEST_URL)?;
        let info: UpdateInfo =
            serde_json::from_str(&body).map_err(|e| format!("bad update manifest: {e}"))?;
        let cur = semver::Version::parse(&current)
            .map_err(|e| format!("bad current version {current}: {e}"))?;
        let new = semver::Version::parse(info.version.trim_start_matches('v'))
            .map_err(|e| format!("bad manifest version {}: {e}", info.version))?;
        Ok(CheckResult {
            current,
            available: (new > cur).then_some(info),
        })
    }
}

/// Download, verify, replace, relaunch. On success this never returns: the
/// replacement is already starting and this process exits.
#[tauri::command]
pub fn update_apply(app: AppHandle, info: UpdateInfo) -> Result<(), String> {
    #[cfg(not(windows))]
    {
        let _ = (app, info);
        return Err(
            "Updates on Linux are delivered by the installed package or AppImage download"
                .to_string(),
        );
    }
    #[cfg(windows)]
    {
        let bytes = http_get_bytes(&info.url)?;
        // Order matters more here than anywhere else in the app: verify before the
        // bytes touch the disk, so a failed check leaves nothing behind to run.
        verify_signature(&bytes, &info.signature)?;
        self_replace_and_relaunch(&app, &bytes)
    }
}

/// The whole security model. A download that fails this is discarded.
#[cfg(windows)]
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
    let sig = minisign_verify::Signature::decode(sig_text)
        .map_err(|e| format!("parse signature: {e}"))?;
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
#[cfg(windows)]
fn self_replace_and_relaunch(app: &AppHandle, new_bytes: &[u8]) -> Result<(), String> {
    let cur = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let old = aside_path(&cur);
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

/// Where to rename `exe` aside so a new copy can take its path: `greedout.old`,
/// after deleting any stale one. A process started before an earlier update can
/// still be running from that `.old`, which locks it; the delete then fails and
/// renaming onto it fails with Access denied, so step aside to `greedout.old-<pid>`
/// instead. `cleanup_old` removes both once nothing runs from them.
#[cfg(any(windows, test))]
pub(crate) fn aside_path(exe: &Path) -> PathBuf {
    let old = exe.with_extension("old");
    let _ = std::fs::remove_file(&old);
    if old.exists() {
        exe.with_extension(format!("old-{}", std::process::id()))
    } else {
        old
    }
}

/// Delete the copies earlier updates and reinstalls left behind. Best effort and
/// retried every launch, because a file stays locked while anything still runs
/// from it. Called before the app builds anything.
pub fn cleanup_old() {
    if let Ok(cur) = std::env::current_exe() {
        cleanup_old_beside(&cur);
    }
}

/// `greedout.old` and every `greedout.old-<pid>` next to `exe`.
fn cleanup_old_beside(exe: &Path) {
    let old = exe.with_extension("old");
    if old.exists() {
        let _ = std::fs::remove_file(&old);
    }
    let (Some(dir), Some(stem)) = (exe.parent(), exe.file_stem().and_then(|s| s.to_str())) else {
        return;
    };
    let prefix = format!("{stem}.old-");
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(&prefix))
            {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

#[cfg(windows)]
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

#[cfg(windows)]
fn http_get_string(url: &str) -> Result<String, String> {
    String::from_utf8(http_get_bytes(url)?).map_err(|e| format!("response not utf8: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("greedout-{name}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn aside_path_reuses_old_after_deleting_a_stale_one() {
        let dir = scratch("aside-stale");
        let exe = dir.join("greedout.exe");
        assert_eq!(aside_path(&exe), dir.join("greedout.old"));
        std::fs::write(dir.join("greedout.old"), b"stale").unwrap();
        assert_eq!(aside_path(&exe), dir.join("greedout.old"));
        assert!(!dir.join("greedout.old").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An exclusive handle stands in for a process still running from `.old`:
    /// both make the delete fail, so the aside name must move off `.old`.
    #[cfg(windows)]
    #[test]
    fn aside_path_steps_past_a_locked_old() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = scratch("aside-locked");
        let exe = dir.join("greedout.exe");
        let old = dir.join("greedout.old");
        std::fs::write(&old, b"running").unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&old)
            .unwrap();
        let got = aside_path(&exe);
        assert_eq!(
            got,
            dir.join(format!("greedout.old-{}", std::process::id()))
        );
        assert!(old.exists());
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_removes_old_and_numbered_old_only() {
        let dir = scratch("cleanup");
        let exe = dir.join("greedout.exe");
        for n in [
            "greedout.exe",
            "greedout.old",
            "greedout.old-1234",
            "greedout.old-99",
            "greedout.older",
            "other.old-1",
        ] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        cleanup_old_beside(&exe);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["greedout.exe", "greedout.older", "other.old-1"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
