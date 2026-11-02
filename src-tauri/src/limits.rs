//! Plan-limit dials: how much of the 5-hour and weekly allowance is used, against
//! how far through each window we are.
//!
//! Transcripts never record the real percentage, so usage is estimated: spend
//! inside the window divided by a dollar budget. The budget is learned from the
//! user's own `/usage` reports (copied, then read by clicking the dials): each
//! report pairs a real percentage with the local spend at that moment, and the
//! budget is fitted across the saved pairs. Until one exists the dials show
//! elapsed time only.
//!
//! Per-turn costs come from the conversation ledger, which re-reads only the
//! transcript lines added since the last run.

use crate::config;
use crate::ledger::{self, Windows, D7_MS, H5_MS};
use crate::scan;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Saved calibration pairs per window, and how long one stays useful.
const KEEP_PAIRS: usize = 10;
const PAIR_TTL_MS: i64 = 30 * 24 * 3600 * 1000;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitWindow {
    /// Estimated share used: window spend / budget, 0..1+. None until calibrated.
    pub pct: Option<f64>,
    /// Spend inside the window, USD.
    pub cost: f64,
    /// The budget the estimate divides by, USD; 0 until calibrated.
    pub budget: f64,
    /// Fraction of the window elapsed, 0..1; None when the window start is unknown.
    pub elapsed: Option<f64>,
    /// Window end, epoch ms.
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub five_hour: LimitWindow,
    pub weekly: LimitWindow,
}

/// One `/usage` reading: the real share used, and local spend over the same window
/// up to the moment the report was made.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pair {
    cost: f64,
    pct: f64,
    at: i64,
    /// The window's end, epoch ms; 0 on readings saved before this was kept.
    #[serde(default)]
    reset: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Calib {
    five: Vec<Pair>,
    week: Vec<Pair>,
    /// Latest reset instants read off a report, epoch ms. The weekly one is a fixed
    /// anchor rolled forward in whole weeks; the 5-hour one pins the live window.
    five_reset: i64,
    week_reset: i64,
}

fn calib_path() -> PathBuf {
    config::app_dir().join("limit_calibration.json")
}

fn load_calib() -> Calib {
    std::fs::read_to_string(calib_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_calib(c: &Calib) -> Result<(), String> {
    let _ = std::fs::create_dir_all(config::app_dir());
    let text = serde_json::to_string_pretty(c).map_err(|e| e.to_string())?;
    std::fs::write(calib_path(), text).map_err(|e| format!("could not save calibration: {e}"))
}

/// Least-squares budget through the origin (cost = budget * pct), so readings at a
/// high percentage, where rounding matters least, count most. 0 when there is none.
fn fit_budget(pairs: &[Pair]) -> f64 {
    let (num, den) = pairs.iter().fold((0.0, 0.0), |(n, d), p| (n + p.cost * p.pct, d + p.pct * p.pct));
    if den > 0.0 {
        num / den
    } else {
        0.0
    }
}

static LATEST: Mutex<Option<Limits>> = Mutex::new(None);

pub fn latest() -> Limits {
    LATEST.lock().ok().and_then(|g| g.clone()).unwrap_or_default()
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Spend of the open windows, from the ledger after it catches up with the
/// conversations. `calib` supplies the resets a `/usage` report pinned.
fn gather(now: i64, calib: &Calib) -> Windows {
    ledger::windows(now, calib.five_reset, calib.week_reset).unwrap_or_default()
}

/// While the window of the newest `/usage` reading is still open, the dial starts
/// from that reading and moves by the spend since at the fitted budget, so it shows
/// the reported share right after a paste. Otherwise spend over the fitted budget.
fn window(cost: f64, pairs: &[Pair], end: Option<i64>, len: i64, now: i64) -> LimitWindow {
    let fitted = fit_budget(pairs);
    let anchor = pairs.last().filter(|p| end.is_some_and(|e| e == p.reset));
    let pct = match anchor {
        Some(a) if fitted > 0.0 => Some((a.pct + (cost - a.cost) / fitted).max(0.0)),
        _ => (fitted > 0.0).then(|| cost / fitted),
    };
    // The budget the shown share implies, so cost / budget always reads as the dial.
    let budget = pct.filter(|p| *p > 0.0).map_or(fitted, |p| cost / p);
    LimitWindow {
        pct,
        cost,
        budget,
        elapsed: end.map(|e| ((now - (e - len)) as f64 / len as f64).clamp(0.0, 1.0)),
        resets_at: end,
    }
}

/// Estimate both windows from local conversations and the saved calibration. The
/// ledger decides where each window ends (a pinned reset, else the activity-based
/// 5-hour window, else a trailing week) and keeps its running spend.
fn estimate(now: i64) -> Limits {
    let calib = load_calib();
    estimate_from(&gather(now, &calib), &calib, now)
}

fn estimate_from(w: &Windows, calib: &Calib, now: i64) -> Limits {
    Limits {
        five_hour: window(w.five.cost, &calib.five, w.five.end, H5_MS, now),
        weekly: window(w.seven.cost, &calib.week, w.seven.end, D7_MS, now),
    }
}

/// What a `/usage` report says about one limit: share used (0..1) and window end.
struct Reading {
    pct: f64,
    reset: i64,
}

struct Report {
    at: Option<i64>,
    five: Option<Reading>,
    week: Option<Reading>,
}

/// Read the "Plan limits" lines of a copied `/usage` report:
/// `- session-0: 25% (resets 2026-11-01T06:19:59.632633+00:00)` for the 5-hour
/// limit and `- weekly_all-1: ...` for the weekly one. Other limit lines (per-model
/// weekly caps) are ignored.
fn parse_report(text: &str) -> Option<Report> {
    let at = text
        .lines()
        .find(|l| l.contains("usage report"))
        .and_then(|l| l.split_once('(').map(|(_, r)| r.trim_end_matches(')').trim()))
        .and_then(scan::iso_to_millis);
    let (mut five, mut week) = (None, None);
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("- ") else { continue };
        let Some((name, rest)) = rest.split_once(':') else { continue };
        let slot = if name.starts_with("session") {
            &mut five
        } else if name.starts_with("weekly_all") {
            &mut week
        } else {
            continue;
        };
        let Some((pct, rest)) = rest.trim().split_once('%') else { continue };
        let Ok(pct) = pct.trim().parse::<f64>() else { continue };
        let Some(reset) = rest
            .split_once("resets")
            .map(|(_, r)| r.trim().trim_end_matches(')').trim())
            .and_then(scan::iso_to_millis)
        else {
            continue;
        };
        *slot = Some(Reading { pct: pct / 100.0, reset });
    }
    (five.is_some() || week.is_some()).then_some(Report { at, five, week })
}

/// Fold a pasted `/usage` report into the saved calibration.
fn calibrate(text: &str, now: i64) -> Result<Calib, String> {
    let report = parse_report(text).ok_or("not a /usage report")?;
    let at = report.at.unwrap_or(now);
    if now - at > D7_MS {
        return Err("that /usage report is too old".into());
    }
    // Spend inside the report's window, up to the moment the report was made.
    let spent = |end: i64, len: i64| -> f64 { ledger::spent(end - len, end.min(at + 1)) };
    let mut calib = load_calib();
    if let Some(r) = &report.five {
        calib.five_reset = r.reset;
        if r.pct > 0.0 {
            calib.five.push(Pair { cost: spent(r.reset, H5_MS), pct: r.pct, at, reset: r.reset });
        }
    }
    if let Some(r) = &report.week {
        calib.week_reset = r.reset;
        if r.pct > 0.0 {
            calib.week.push(Pair { cost: spent(r.reset, D7_MS), pct: r.pct, at, reset: r.reset });
        }
    }
    for pairs in [&mut calib.five, &mut calib.week] {
        pairs.retain(|p| now - p.at < PAIR_TTL_MS);
        // Pasting the same report again replaces its reading rather than adding a copy.
        pairs.reverse();
        let mut seen = HashSet::new();
        pairs.retain(|p| seen.insert(p.at));
        pairs.reverse();
        let extra = pairs.len().saturating_sub(KEEP_PAIRS);
        pairs.drain(..extra);
    }
    save_calib(&calib)?;
    Ok(calib)
}

fn publish(app: &tauri::AppHandle, out: &Limits) {
    use tauri::Emitter;
    if let Ok(mut g) = LATEST.lock() {
        *g = Some(out.clone());
    }
    let _ = app.emit("limits", out);
}

/// Read the clipboard once, on the user's click, and calibrate from it.
pub fn calibrate_from_clipboard(app: &tauri::AppHandle) -> Result<Limits, String> {
    let text = arboard::Clipboard::new()
        .and_then(|mut c| c.get_text())
        .map_err(|_| "could not read the clipboard".to_string())?;
    let now = now_ms();
    let started = std::time::Instant::now();
    // Catch the ledger up first, so the report is compared with current spend.
    let _ = gather(now, &load_calib());
    let scanned = started.elapsed();
    calibrate(&text, now)?;
    let out = estimate(now);
    config::debug_log(&config::load_config(), &format!("limits calibrate: scan {scanned:?}, total {:?}", started.elapsed()));
    publish(app, &out);
    Ok(out)
}

/// Forget every saved `/usage` reading, so both dials are uncalibrated again until
/// the next report is pasted (one report calibrates both).
pub fn clear(app: &tauri::AppHandle) -> Result<Limits, String> {
    save_calib(&Calib::default())?;
    let out = estimate(now_ms());
    publish(app, &out);
    Ok(out)
}

/// Background loop: refresh the estimate (cheap, cached per file), publishing each
/// result as the "limits" event.
pub fn spawn(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        let out = estimate(now_ms());
        config::debug_log(
            &config::load_config(),
            &format!(
                "limits 5h pct={:?} cost={:.2} budget={:.2} end={:?} el={:?} | 7d pct={:?} cost={:.2} budget={:.2} end={:?} el={:?}",
                out.five_hour.pct,
                out.five_hour.cost,
                out.five_hour.budget,
                out.five_hour.resets_at,
                out.five_hour.elapsed,
                out.weekly.pct,
                out.weekly.cost,
                out.weekly.budget,
                out.weekly.resets_at,
                out.weekly.elapsed
            ),
        );
        publish(&app, &out);
        std::thread::sleep(Duration::from_secs(20));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = "Claude Code usage report (2026-11-01T04:37:03.116Z)\nClient: x\n\nPlan limits:\n- session-0: 25% (resets 2026-11-01T06:19:59.632633+00:00)\n- weekly_all-1: 39% (resets 2026-11-05T17:59:59.632650+00:00)\n- weekly_fable-2: 5% (resets 2026-11-05T17:59:59+00:00)\n";

    #[test]
    fn report_reads_both_limits_and_ignores_the_rest() {
        let r = parse_report(REPORT).unwrap();
        assert_eq!(r.at, scan::iso_to_millis("2026-11-01T04:37:03.116Z"));
        let (f, w) = (r.five.unwrap(), r.week.unwrap());
        assert!((f.pct - 0.25).abs() < 1e-9 && (w.pct - 0.39).abs() < 1e-9);
        assert_eq!(f.reset, scan::iso_to_millis("2026-11-01T06:19:59.632Z").unwrap());
        assert!(parse_report("hello").is_none());
    }

    #[test]
    fn budget_fit_weights_high_percentages() {
        assert_eq!(fit_budget(&[]), 0.0);
        let p = |cost, pct| Pair { cost, pct, at: 0, reset: 0 };
        assert!((fit_budget(&[p(12.5, 0.25)]) - 50.0).abs() < 1e-9);
        // A noisy low reading barely moves a clean high one.
        let b = fit_budget(&[p(50.0, 1.0), p(15.0, 0.25)]);
        assert!((b - 50.0).abs() < 2.0, "{b}");
    }

    #[test]
    fn dial_starts_at_the_newest_reading_while_its_window_is_open() {
        let p = |cost, pct, reset| Pair { cost, pct, at: 0, reset };
        // Older readings imply a $50 budget; the newest, $60.
        let pairs = [p(10.0, 0.2, 1), p(30.0, 0.5, 2)];
        let budget = fit_budget(&pairs);
        // Same window: exactly the reported share at the reported spend.
        let w = window(30.0, &pairs, Some(2), H5_MS, 0);
        assert!((w.pct.unwrap() - 0.5).abs() < 1e-9);
        // Then it moves by the fitted budget.
        let w = window(35.0, &pairs, Some(2), H5_MS, 0);
        assert!((w.pct.unwrap() - (0.5 + 5.0 / budget)).abs() < 1e-9);
        assert!((w.cost / w.budget - w.pct.unwrap()).abs() < 1e-9);
        // A different window falls back to spend over the fitted budget.
        let w = window(35.0, &pairs, Some(3), H5_MS, 0);
        assert!((w.pct.unwrap() - 35.0 / budget).abs() < 1e-9);
    }
}
