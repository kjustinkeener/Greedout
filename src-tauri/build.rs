fn main() {
    // Stamp the build date so About can show it. Strangers filing bugs report a
    // version; a version plus a date tells you which build they actually have,
    // which matters while releases are frequent. Dependency-free: seconds since
    // the epoch, formatted in the frontend.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=GREEDOUT_BUILD_EPOCH={secs}");
    tauri_build::build()
}
