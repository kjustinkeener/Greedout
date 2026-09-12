# Changelog

All notable changes to Greedout are recorded here. This project follows
[Semantic Versioning](https://semver.org). Releases at or before v0.2.3 are
documented in the GitHub Releases notes and the `v*` git tags.

## [0.2.4] - Unreleased

### Added
- Codex (OpenAI) session support alongside Claude Code. Codex sessions appear in
  the live gauges, Browse, Daily Spend, the Context Explorer, and search:
  - Real per-session context window with a compaction reset, and a neutral
    placeholder (not 0%) with cumulative spend preserved across the
    just-compacted gap.
  - Per-model pricing and full display names for the GPT-5 family (including mini
    and nano tiers) and newer releases.
  - An estimated Context Explorer breakdown with chat drill-in.
  - Cross-harness focus detection, including pinning the focused thread from the
    desktop app.
- Cross-harness chat search: search now covers Codex sessions as well as Claude,
  and each hit is badged with its harness.
- An FTS5 trigram search index for instant substring search, plus a parallel
  scan with raw pre-filtering to speed searching.
- Hover tooltips on every Settings control, translated into all 13 locales.
- Per-window geometry: every resizable child window remembers its size and
  position and is clamped back on screen when reopened.
- Pricing and labeling for Claude Fable 5.1.
- A real automated test suite (Rust backend unit tests and frontend vitest)
  wired into CI, so both suites gate every change. Backend coverage grew to 85
  tests, including the Context Explorer breakdown parser and previously untested
  scan, Codex, and browse helpers.

### Changed
- A single token budget drives the gauges for every session across both
  harnesses.
- Settings reworked: removed the max-sessions cap, added a per-harness gauge
  toggle and a clear-cache button, and themed the controls.
- Scan progress is now one shared component across the Daily Spend and Context
  Explorer windows, so the two no longer diverge. It shows three labeled phase
  bars (Locating sessions, Parsing transcripts, Saving) with a count and elapsed
  time per phase; the theme gauge gradient sweeps once across the three bars and
  elapsed switches to seconds past one second.
- Daily Spend groups lanes by full session path and badges each lane with its
  harness.
- The About window was redesigned to match the launcher: borderless with
  full-panel drag, an in-panel close, resized to fit, and a retranslated tagline
  in all 13 locales.
- README trimmed (dropped the stale "no release yet" clause and tightened the
  download paragraph).

### Fixed
- Clearing the cache no longer leaves the Daily Spend and Context Explorer
  windows unable to refresh: the schema-ready flag is reset when the database is
  removed, so the next scan rebuilds the tables. The Context Explorer also
  auto-rebuilds its index when it is enabled but empty after a clear.
- Two crashes when decoding oddly named paths: the Codex session-id and the
  project-folder decoders sliced byte offsets that could split a multibyte UTF-8
  character, so a single unusually named file or folder could abort a scan.
- A Daily Spend crash (each_key_duplicate) when two lanes shared a leaf name.
- Removed a scan log whose per-line timing and coverage were misleading (it only
  sampled one item in N).

### Notes
- The source tree is kept free of U+2014 (em dash) in committed content.
