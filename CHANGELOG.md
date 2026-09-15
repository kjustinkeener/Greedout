# Changelog

All notable changes to Greedout are recorded here. This project follows
[Semantic Versioning](https://semver.org). Releases at or before v0.2.3 are
documented in the GitHub Releases notes and the `v*` git tags.

## [0.2.9] - 2026-09-14

### Added
- Cursor is now a supported harness, alongside Claude Code and Codex. Greedout
  reads Cursor's global chat database and shows each Cursor conversation as a live
  gauge: the context fill is Cursor's own real context-window meter, and spend is
  an estimate (Cursor does not persist per-turn token counts on current builds).
  Cursor conversations also appear in Browse, the Context Explorer and chat search,
  and each conversation is filed under its real workspace folder when Cursor has one
  open (conversations with no folder group under a "Cursor" bucket).
- When a Cursor window is frontmost, Greedout pins the conversation you last ran a
  turn in as the focused gauge, the same OS-foreground arbitration already used to
  choose between Claude Code and Codex.

### Changed
- Grok pricing and context window updated to grok-4.6 ($2/M input, $6/M output,
  500K window).

## [0.2.8] - 2026-09-12

### Added
- In-app theme editor. Duplicate a built-in theme or clone one of your own, then
  edit its background, token colors, gauge gradient, translucent surfaces and
  spend text with a live preview, and save it as a named custom theme. Custom
  themes group under "My Themes" at the top of the picker, persist to a sidecar
  file, and paint on cold start with no dark flash. Names that collide with a
  built-in or another custom theme are blocked.
- Every secondary window (Context Explorer, Daily Spend, Settings, About, the
  theme and font pickers) is now borderless and transparent like the main
  window: it rests at the Opacity level and snaps opaque on hover, easing back
  over two seconds. Context Explorer and Daily Spend gained a branded title bar
  with minimize, maximize/restore and close buttons.
- Context Explorer can size the browse-level tiles (harness, project and session
  lists) by measured spend, with a bytes/$ toggle in the toolbar, and a
  Best/Recent sort beside the search match count.

### Changed
- The CPU status-bar bars now follow the active theme, drawn as a bottom slice of
  the gauge gradient, and recolor instantly on a theme switch.
- The Context Explorer search toolbar was reworked: the rerun button moved inside
  the search box and the project filter pills moved to their own line.
- The Settings theme button shows a custom theme's name rather than its slug.
- Title-bar maximize buttons use proper maximize and restore-down glyphs that
  track whether the window is maximized.

## [0.2.7] - 2026-09-12

### Added
- The recent-compaction strip now appears for Codex sessions too. Codex writes no
  post-compact size in the boundary record and encrypts the summary, so the strip
  fills in one turn later (once the post size exists) and its trailing label reads
  "summary encrypted" instead of a reader link, but the pre and post sizes and the
  time since the compaction are shown the same as for Claude Code.

### Changed
- The full project path is now a tooltip on the project name only, instead of on
  the whole session row or panel. The session-title and project tooltips are
  labeled ("Session: ..." and "Project: ..."), each keeping its rename or explore
  hint on a second line. The Codex "summary encrypted" strip has an explanatory
  tooltip.

## [0.2.6] - 2026-09-12

### Added
- Recent-compaction strip on the focused gauge: right after a `/compact`, a
  full-width band under the readout shows the pre and post context size and how
  long ago it happened, and stays for the next three prompts. Click it to open a
  reader window with the full compaction summary text. Claude Code only; Codex
  compaction summaries are encrypted on disk and have no strip.

## [0.2.5] - 2026-09-12

### Added
- Live search in the Context Explorer: results update as you type once the
  query reaches three characters (the trigram index floor), instead of only on
  Enter. Shorter queries still search on Enter through the disk-scan path.

### Changed
- The live-search debounce is adaptive: it ratchets up to the slowest search
  roundtrip seen so far (clamped to a usable range), so an instant index stays
  responsive while a cache that leans on the slower disk scan backs off on its
  own rather than relaunching on every keystroke.

## [0.2.4] - 2026-09-11

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
