//! Self-installer: the app installs itself, with no NSIS, MSI or Inno step.
//!
//! The deliverable is one raw exe. Run it from anywhere outside the install
//! directory and it shows an install card instead of the gauge window; run it
//! from inside, and it is the app. `--uninstall` makes it the uninstaller, which
//! is what the Add/Remove Programs entry invokes.
//!
//! It installs to `%LOCALAPPDATA%\Greedout` for two reasons, and the second is
//! the one that matters later: a per-user directory needs no elevation, and it
//! is the same directory a future updater self-replaces into, so first install
//! and every update are the same file operation on the same target.
//!
//! Deliberately dependency-free: PowerShell for shortcuts, `reg` for the
//! registry. Both ship with Windows and neither needs a crate.

use serde::Serialize;
#[cfg(windows)]
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

const APP_NAME: &str = "Greedout";
const EXE_NAME: &str = "greedout.exe";

/// Spawn without flashing a console window. Every helper process here is
/// invisible plumbing; a black box appearing mid-install reads as a crash.
#[cfg(windows)]
fn hidden(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW)
}

#[cfg(not(windows))]
fn hidden(cmd: &mut Command) -> &mut Command {
    cmd
}

/// `%LOCALAPPDATA%\Greedout`.
pub fn install_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join(APP_NAME))
}

/// Are we running from inside the install directory?
///
/// Both sides are canonicalized. Without it, `..` segments, drive-letter casing
/// and 8.3 short paths all make `starts_with` say no to a path that is in fact
/// the install directory.
#[cfg(windows)]
fn is_installed() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let Some(dir) = install_dir() else {
        return false;
    };
    let exe = exe.canonicalize().unwrap_or(exe);
    let dir = dir.canonicalize().unwrap_or(dir);
    exe.starts_with(dir)
}

#[cfg(not(windows))]
fn is_installed() -> bool {
    false
}

/// Show the install card instead of the app. Never true in a dev build, so the
/// card cannot be seen under `tauri dev`: build release and run it from
/// somewhere other than the install directory.
pub fn needs_setup() -> bool {
    cfg!(windows) && !cfg!(debug_assertions) && !is_installed()
}

/// What the frontend needs to decide which UI to mount, and what to put on the
/// card. One round trip, because it is on the launch path.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupState {
    pub needs_setup: bool,
    pub installed: bool,
    /// A previous install exists on disk, so the card can say "update" rather
    /// than "install" instead of telling the user a lie about a fresh machine.
    pub existing: bool,
    pub version: String,
    pub install_dir: String,
}

#[tauri::command]
pub fn setup_state() -> SetupState {
    let dir = install_dir();
    SetupState {
        needs_setup: needs_setup(),
        installed: cfg!(windows) && is_installed(),
        existing: cfg!(windows)
            && dir
                .as_ref()
                .map(|d| d.join(EXE_NAME).exists())
                .unwrap_or(false),
        version: env!("CARGO_PKG_VERSION").to_string(),
        install_dir: dir.map(|d| d.display().to_string()).unwrap_or_default(),
    }
}

/// Copy ourselves into place, make the shortcuts, and register with Add/Remove
/// Programs. Returns the installed exe's path for the caller to launch.
#[tauri::command]
pub fn perform_install(desktop_shortcut: bool) -> Result<String, String> {
    #[cfg(not(windows))]
    {
        let _ = desktop_shortcut;
        return Err("Linux builds are installed through their .deb package or AppImage, not an in-app installer".to_string());
    }
    #[cfg(windows)]
    {
        let dir = install_dir().ok_or("no LOCALAPPDATA")?;
        let src = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("create install dir: {e}"))?;
        let target = dir.join(EXE_NAME);

        // Copying a file onto itself truncates it. Reachable in the one case that
        // looks harmless: someone runs the already-installed exe by hand.
        let same = src.canonicalize().unwrap_or_else(|_| src.clone())
            == target.canonicalize().unwrap_or_else(|_| target.clone());
        if !same {
            std::fs::copy(&src, &target).map_err(|e| format!("copy exe: {e}"))?;
        }

        if let Some(sm) = start_menu_dir() {
            let _ = create_shortcut(&sm.join(format!("{APP_NAME}.lnk")), &target);
        }
        if desktop_shortcut {
            if let Some(d) = desktop_dir() {
                let _ = create_shortcut(&d.join(format!("{APP_NAME}.lnk")), &target);
            }
        }
        register_uninstall(&dir, &target);
        Ok(target.display().to_string())
    }
}

/// Hand off to the installed copy and quit.
#[tauri::command]
pub fn launch_installed_and_exit(app: tauri::AppHandle, exe: String) {
    let _ = hidden(&mut Command::new(&exe)).spawn();
    app.exit(0);
}

/// Single-quoted PowerShell strings escape a quote by doubling it. Every path
/// interpolated into a script goes through this, because a directory with an
/// apostrophe in it is legal and would otherwise end the string early.
#[cfg(windows)]
fn ps_quote(p: &Path) -> String {
    p.display().to_string().replace('\'', "''")
}

#[cfg(windows)]
fn powershell(script: &str) -> Result<(), String> {
    hidden(&mut Command::new("powershell"))
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .status()
        .map(|_| ())
        .map_err(|e| format!("powershell: {e}"))
}

/// Shortcuts are COM objects, and the shell already exposes the COM object. A
/// crate for this would be a dependency to write six lines of script.
#[cfg(windows)]
fn create_shortcut(lnk: &Path, target: &Path) -> Result<(), String> {
    let dir = target.parent().unwrap_or(target);
    powershell(&format!(
        "$w = New-Object -ComObject WScript.Shell; $s = $w.CreateShortcut('{lnk}'); \
         $s.TargetPath='{tgt}'; $s.WorkingDirectory='{dir}'; \
         $s.IconLocation='{tgt},0'; $s.Description='{APP_NAME}'; $s.Save()",
        lnk = ps_quote(lnk),
        tgt = ps_quote(target),
        dir = ps_quote(dir),
    ))
}

#[cfg(windows)]
fn start_menu_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(r"Microsoft\Windows\Start Menu\Programs"))
}

#[cfg(windows)]
fn desktop_dir() -> Option<PathBuf> {
    dirs::desktop_dir()
}

#[cfg(windows)]
fn uninstall_key() -> String {
    format!(r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\{APP_NAME}")
}

#[cfg(windows)]
fn reg_add(name: &str, kind: &str, data: &str) {
    let _ = hidden(&mut Command::new("reg"))
        .args([
            "add",
            &uninstall_key(),
            "/v",
            name,
            "/t",
            kind,
            "/d",
            data,
            "/f",
        ])
        .status();
}

/// Bytes on disk, for the size column. Walks the tree because the exe stops
/// being the only thing in the directory as soon as anything caches beside it.
#[cfg(windows)]
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            _ => e.metadata().map(|m| m.len()).unwrap_or(0),
        })
        .sum()
}

/// yyyyMMdd from the unix epoch, civil-from-days. A date crate for one value in
/// one registry write is not worth the dependency.
#[cfg(windows)]
fn install_date() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}")
}

/// The Add/Remove Programs row. Under HKCU, which is what keeps the whole
/// install free of an elevation prompt.
///
/// The optional-looking values are not decoration: without EstimatedSize the row
/// shows a blank size, and without InstallDate it sorts oddly under "Date
/// installed". A row that looks half filled in reads as a bad install.
///
/// Nothing here is fatal. Failing to write a registry value should not undo a
/// successful copy, so these are best effort and the install still succeeds.
#[cfg(windows)]
fn register_uninstall(dir: &Path, target: &Path) {
    let exe = target.display().to_string();
    // The one value that makes the Uninstall button do anything.
    let cmd = format!("\"{exe}\" --uninstall");
    reg_add("DisplayName", "REG_SZ", APP_NAME);
    reg_add("DisplayVersion", "REG_SZ", env!("CARGO_PKG_VERSION"));
    reg_add("Publisher", "REG_SZ", "Justin Keener");
    reg_add("DisplayIcon", "REG_SZ", &exe);
    reg_add("InstallLocation", "REG_SZ", &dir.display().to_string());
    reg_add("UninstallString", "REG_SZ", &cmd);
    // Uninstall is already non-interactive, so the quiet form is the same string.
    reg_add("QuietUninstallString", "REG_SZ", &cmd);
    reg_add(
        "EstimatedSize",
        "REG_DWORD",
        &(dir_size(dir) / 1024).to_string(),
    );
    reg_add("InstallDate", "REG_SZ", &install_date());
    reg_add("NoModify", "REG_DWORD", "1");
    reg_add("NoRepair", "REG_DWORD", "1");
}

#[cfg(windows)]
fn remove_shortcuts() {
    if let Some(sm) = start_menu_dir() {
        let _ = std::fs::remove_file(sm.join(format!("{APP_NAME}.lnk")));
    }
    if let Some(d) = desktop_dir() {
        let _ = std::fs::remove_file(d.join(format!("{APP_NAME}.lnk")));
    }
}

#[cfg(windows)]
fn remove_uninstall_key() {
    let _ = hidden(&mut Command::new("reg"))
        .args(["delete", &uninstall_key(), "/f"])
        .status();
}

/// Uninstall, then exit. Never returns.
///
/// A process cannot delete the directory it is running from, so the deletion is
/// handed to a detached PowerShell job that stops every copy of the app, waits,
/// and retries for about thirty seconds. It retries because a single attempt
/// races our own exit, and it is PowerShell rather than `cmd /c` because `cmd`
/// strips the outer quotes it is handed, unbalancing a quoted path: it then
/// deletes nothing and, being hidden and detached, says nothing about it.
///
/// If files survive, Windows shows "This program might not have uninstalled
/// correctly". That dialog reads the filesystem, not the registry, so removing
/// the Add/Remove entry does not silence it. It is a symptom; fix the deletion.
#[cfg(windows)]
pub fn run_uninstall() -> ! {
    remove_shortcuts();
    remove_uninstall_key();
    if let Some(dir) = install_dir() {
        let script = format!(
            "Get-Process -Name '{proc}' -ErrorAction SilentlyContinue | Stop-Process -Force; \
             Start-Sleep -Milliseconds 400; \
             for ($i = 0; $i -lt 60; $i++) {{ \
               try {{ Remove-Item -LiteralPath '{dir}' -Recurse -Force -ErrorAction Stop; break }} \
               catch {{ Start-Sleep -Milliseconds 500 }} }}",
            proc = APP_NAME.to_lowercase(),
            dir = ps_quote(&dir),
        );
        let _ = hidden(&mut Command::new("powershell"))
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .spawn();
    }
    std::process::exit(0);
}
