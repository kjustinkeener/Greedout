# Changelog

All notable changes to Greedout are recorded here. This project follows
[Semantic Versioning](https://semver.org). Releases at or before v0.2.3 are
documented in the GitHub Releases notes and the `v*` git tags.

## [0.2.4] - Unreleased

### Added
- Window geometry is remembered per window: every resizable child window saves
  its size and position and is clamped back on screen when reopened.
- Backend unit tests expanded from 55 to 85, including a new test module for the
  Context Explorer breakdown parser and coverage of previously untested pure
  functions in the scan, Codex, and browse paths. CI runs the Rust and frontend
  test suites, so these gate every change.

### Changed
- Scan progress is a single shared component across the Daily Spend and Context
  Explorer windows, so the two no longer diverge. It shows three labeled phase
  bars (Locating sessions, Parsing transcripts, Saving) with a count and elapsed
  time per phase, and the theme gauge gradient sweeps once across the three bars.
  Elapsed switches to seconds past one second.
- The About window was redesigned to match the launcher styling.
- The debug log is written to the app data folder and truncated on launch.

### Fixed
- Clearing the cache no longer leaves the Daily Spend and Context Explorer
  windows unable to refresh: the schema-ready flag is reset when the database is
  removed, so the next scan rebuilds the tables instead of hitting a table-less
  database.
- Two crashes when decoding oddly named paths: the Codex session-id and the
  project-folder decoders sliced byte offsets that could split a multibyte UTF-8
  character. A single unusually named file or folder could previously abort a
  scan.
- Removed a scan log whose per-line timing and coverage were misleading (it only
  sampled one item in N).

### Notes
- The source tree is kept free of U+2014 (em dash) in committed content.
