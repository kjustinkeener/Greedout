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
//! registry writes. Both ship with Windows and neither needs a crate. The one
//! registry read goes through `winreg`, which the app already carries for the
//! font list, rather than parsing `reg query` output.

use serde::Serialize;
#[cfg(any(windows, test))]
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
pub(crate) fn is_installed() -> bool {
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
pub(crate) fn is_installed() -> bool {
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
    /// The installed copy is a newer version than this exe, so the card offers
    /// to open it instead of installing over it. Installing an older build over
    /// a newer one would hand the newer one's settings to a build that predates
    /// them. False whenever the installed version cannot be read, which keeps the
    /// card exactly as it was.
    pub installed_newer: bool,
    /// The installed version, from the Add/Remove Programs row. Only read while
    /// the card is showing; empty otherwise or when unreadable.
    pub installed_version: String,
    /// The installed exe, for the card's "Open" button to hand off to.
    pub installed_exe: String,
    pub version: String,
    pub install_dir: String,
}

#[tauri::command]
pub fn setup_state() -> SetupState {
    let dir = install_dir();
    let needs_setup = needs_setup();
    let installed_exe = dir.as_ref().map(|d| d.join(EXE_NAME));
    let existing = cfg!(windows) && installed_exe.as_ref().is_some_and(|e| e.exists());
    let version = env!("CARGO_PKG_VERSION");
    // The registry is only consulted for the card: the installed app calls this
    // on every launch and has no use for it.
    let installed_version = if needs_setup && existing {
        installed_display_version().unwrap_or_default()
    } else {
        String::new()
    };
    SetupState {
        needs_setup,
        installed: cfg!(windows) && is_installed(),
        existing,
        installed_newer: is_newer(&installed_version, version),
        installed_version,
        installed_exe: installed_exe
            .map(|e| e.display().to_string())
            .unwrap_or_default(),
        version: version.to_string(),
        install_dir: dir.map(|d| d.display().to_string()).unwrap_or_default(),
    }
}

/// Is `installed` a strictly newer version than `this`? Anything unparseable,
/// including the empty string for "could not read it", is not newer.
fn is_newer(installed: &str, this: &str) -> bool {
    match (
        semver::Version::parse(installed.trim().trim_start_matches('v')),
        semver::Version::parse(this),
    ) {
        (Ok(i), Ok(t)) => i > t,
        _ => false,
    }
}

/// `DisplayVersion` from our Add/Remove Programs row, which `register_uninstall`
/// writes on every install. None if the key or value is missing or unreadable.
#[cfg(windows)]
fn installed_display_version() -> Option<String> {
    use winreg::enums::HKEY_CURRENT_USER;
    let key = winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(format!(
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{APP_NAME}"
        ))
        .ok()?;
    key.get_value::<String, _>("DisplayVersion").ok()
}

#[cfg(not(windows))]
fn installed_display_version() -> Option<String> {
    None
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
            replace_exe(&src, &target)?;
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

/// Copy `src` over `target`. Windows will not overwrite an exe that any process
/// is running from, but it will rename it, so an existing target is moved aside
/// first (to the updater's aside name, which the next launch cleans up). A failed
/// copy moves it back, so a reinstall never leaves no exe at all.
#[cfg(any(windows, test))]
fn replace_exe(src: &Path, target: &Path) -> Result<(), String> {
    let aside = if target.exists() {
        let aside = crate::update::aside_path(target);
        std::fs::rename(target, &aside).map_err(|e| format!("move installed exe aside: {e}"))?;
        Some(aside)
    } else {
        None
    };
    if let Err(e) = std::fs::copy(src, target) {
        if let Some(aside) = &aside {
            let _ = std::fs::remove_file(target);
            let _ = std::fs::rename(aside, target);
        }
        return Err(format!("copy exe: {e}"));
    }
    Ok(())
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
#[cfg(any(windows, test))]
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
/// handed to a detached PowerShell job that stops every copy of the app running
/// from the install directory (a copy run from anywhere else, such as a dev
/// build, is a separate program and keeps running), waits, and retries for about
/// thirty seconds. It retries because a single attempt races our own exit, and it
/// is PowerShell rather than `cmd /c` because `cmd` strips the outer quotes it is
/// handed, unbalancing a quoted path: it then deletes nothing and, being hidden
/// and detached, says nothing about it.
///
/// If files survive, Windows shows "This program might not have uninstalled
/// correctly". That dialog reads the filesystem, not the registry, so removing
/// the Add/Remove entry does not silence it. It is a symptom; fix the deletion.
///
/// Only the installed copy uninstalls. Run with `--uninstall` from anywhere else
/// (a dev build, a copy in Downloads) it would still delete the real install, so
/// it touches nothing and exits with an error code instead.
#[cfg(windows)]
pub fn run_uninstall() -> ! {
    if !is_installed() {
        std::process::exit(1);
    }
    remove_shortcuts();
    remove_uninstall_key();
    if let Some(dir) = install_dir() {
        let script = uninstall_script(&dir);
        let _ = hidden(&mut Command::new("powershell"))
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .spawn();
    }
    std::process::exit(0);
}

/// PowerShell pipeline yielding every running copy of the app whose exe lives
/// inside `dir`, and nothing else. The trailing separator keeps a folder that
/// merely shares `dir`'s name as a prefix from matching.
#[cfg(any(windows, test))]
fn copy_processes_ps(dir: &Path) -> String {
    format!(
        "$d = '{dir}'.TrimEnd('\\') + '\\'; \
         Get-Process -Name '{proc}' -ErrorAction SilentlyContinue | \
         Where-Object {{ $_.Path -and $_.Path.StartsWith($d, [System.StringComparison]::OrdinalIgnoreCase) }}",
        dir = ps_quote(dir),
        proc = APP_NAME.to_lowercase(),
    )
}

/// The detached uninstall job: stop this install's copies, then delete `dir`.
#[cfg(any(windows, test))]
fn uninstall_script(dir: &Path) -> String {
    format!(
        "{procs} | Stop-Process -Force -ErrorAction SilentlyContinue; \
         Start-Sleep -Milliseconds 400; \
         for ($i = 0; $i -lt 60; $i++) {{ \
           try {{ Remove-Item -LiteralPath '{dir}' -Recurse -Force -ErrorAction Stop; break }} \
           catch {{ Start-Sleep -Milliseconds 500 }} }}",
        procs = copy_processes_ps(dir),
        dir = ps_quote(dir),
    )
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
    fn is_newer_only_for_a_readable_strictly_newer_version() {
        assert!(is_newer("0.5.0", "0.4.3"));
        assert!(is_newer("v0.4.10", "0.4.3"));
        assert!(!is_newer("0.4.3", "0.4.3"));
        assert!(!is_newer("0.4.2", "0.4.3"));
        assert!(!is_newer("", "0.4.3"));
        assert!(!is_newer("garbage", "0.4.3"));
    }

    #[test]
    fn uninstall_script_stops_only_this_folder_then_deletes_it() {
        let dir = Path::new(r"C:\Users\o'brien\AppData\Local\Greedout");
        let s = uninstall_script(dir);
        // The quote is doubled for the single-quoted literal, in both places it appears.
        assert!(
            s.starts_with(r"$d = 'C:\Users\o''brien\AppData\Local\Greedout'.TrimEnd('\') + '\';")
        );
        assert!(s.contains(r"Remove-Item -LiteralPath 'C:\Users\o''brien\AppData\Local\Greedout'"));
        // Stop-Process only ever sees the path-filtered set, never a bare name match.
        let stop = s.find("| Stop-Process").unwrap();
        assert!(s[..stop].contains("Where-Object { $_.Path -and $_.Path.StartsWith($d, "));
        assert!(!s.contains("Stop-Process -Name"));
        assert_eq!(s.matches("Stop-Process").count(), 1);
    }

    /// Run the real filter against a real process: a renamed copy of `ping.exe`
    /// called `greedout.exe`, started (and killed) by this test, inside a temp
    /// folder. The filter must select exactly that PID for its own folder, and
    /// nothing for a folder whose name is only a prefix of it (`Mine's` vs
    /// `Mine's-other`). Read-only otherwise: nothing is stopped.
    #[cfg(windows)]
    #[test]
    fn process_filter_selects_only_processes_inside_the_folder() {
        let base = scratch("uninst");
        let inside = base.join("Mine's");
        let sibling = base.join("Mine's-other");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let sysroot = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let fake = sibling.join(EXE_NAME);
        std::fs::copy(Path::new(&sysroot).join(r"System32\PING.EXE"), &fake).unwrap();
        let mut child = Command::new(&fake)
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let select = |dir: &Path| -> String {
            let out = Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    &format!("{} | ForEach-Object {{ $_.Id }}", copy_processes_ps(dir)),
                ])
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let got_own = select(&sibling);
        let got_prefix = select(&inside);
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(got_own, child.id().to_string());
        assert_eq!(got_prefix, "", "a name-prefix folder must not match");
    }

    #[test]
    fn replace_exe_installs_fresh_and_moves_an_existing_copy_aside() {
        let dir = scratch("replace");
        let src = dir.join("download.exe");
        let target = dir.join(EXE_NAME);
        std::fs::write(&src, b"v1").unwrap();
        replace_exe(&src, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"v1");
        assert!(!dir.join("greedout.old").exists());

        std::fs::write(&src, b"v2").unwrap();
        replace_exe(&src, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"v2");
        assert_eq!(std::fs::read(dir.join("greedout.old")).unwrap(), b"v1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_exe_restores_the_installed_copy_when_the_copy_fails() {
        let dir = scratch("replace-fail");
        let target = dir.join(EXE_NAME);
        std::fs::write(&target, b"installed").unwrap();
        assert!(replace_exe(&dir.join("missing.exe"), &target).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"installed");
        assert!(!dir.join("greedout.old").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
