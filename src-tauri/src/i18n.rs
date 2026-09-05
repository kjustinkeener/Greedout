//! The Rust half of localization: the two strings a webview can never draw.
//!
//! The tray menu exists before any window does and outlives every one of them,
//! so Show and Quit cannot come from the webview catalog. Nothing else belongs
//! here. If a window can draw it, it goes in `src/lib/locales/`, where a
//! translator can see it and the locale gate can check it.

/// Tray labels for a resolved BCP-47 tag. Matching is two-armed on purpose: an
/// exact regional tag first, then the bare language, so `pt-PT` still lands on
/// Portuguese instead of falling through to English.
pub fn tray_labels(tag: &str) -> (&'static str, &'static str) {
    let lower = tag.to_ascii_lowercase();
    match lower.as_str() {
        "zh-hant" | "zh-tw" | "zh-hk" | "zh-mo" => return ("顯示", "結束"),
        _ => {}
    }
    let base = lower.split('-').next().unwrap_or("en");
    match base {
        "de" => ("Anzeigen", "Beenden"),
        "es" => ("Mostrar", "Salir"),
        "fr" => ("Afficher", "Quitter"),
        "it" => ("Mostra", "Esci"),
        "nl" => ("Tonen", "Afsluiten"),
        "pl" => ("Pokaż", "Zakończ"),
        "pt" => ("Mostrar", "Sair"),
        "ru" => ("Показать", "Выход"),
        "tr" => ("Göster", "Çık"),
        "ja" => ("表示", "終了"),
        "ko" => ("보이기", "종료"),
        "zh" => ("显示", "退出"),
        _ => ("Show", "Quit"),
    }
}
