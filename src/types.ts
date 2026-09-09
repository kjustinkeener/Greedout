// Mirror of the Rust `Session` payload emitted on the "sessions" event.
export interface Session {
  id: string;
  project: string; // short project name (last path component)
  projectPath: string; // full decoded path, for tooltip
  projectRoot: string; // directory of the project this session groups under
  subPath: string; // path from the project down to the session's dir; "" if none
  title: string; // customTitle, or custom label override
  subtitle: string; // lastPrompt, truncated
  ctx: number | null; // context tokens; null if no assistant usage yet
  target: number; // gauge budget: the model's sweet-spot token count
  limit: number; // the model's hard enforced context window
  model: string; // short model name (Opus / Sonnet / Haiku / …)
  modelVersion: string; // version from raw id (e.g. "4.8"); empty if unknown
  pct: number | null; // ctx / target, 0..1
  live: boolean; // mtime within ~2x poll = actively responding
  mtime: number; // unix seconds
  sizeBytes: number; // transcript .jsonl size on disk
  costUsd: number; // cumulative estimated spend so far, USD
  focused: boolean; // open in the Claude app right now (transient; bold + pinned top)
}

// One time-sample of a session, accumulated client-side for the history graph.
export interface Sample {
  t: number; // wall-clock ms when sampled
  ctx: number | null; // context tokens at that moment
  cost: number; // cumulative spend (USD) at that moment
}

// One billed assistant turn, for the Daily Spend window. Deduped across all
// transcripts by the backend, so summing costs never double-counts resumes.
export interface SpendEvent {
  t: number; // turn timestamp, epoch ms
  cost: number; // this turn's estimated USD cost
  session: string; // session id it was first seen in
  project: string; // readable project name
  title: string; // session title (custom title/label), "" if none yet
  mtime: number; // session's overall last-activity time (transcript mtime), epoch ms
}

// What the history graph reports while a line is hovered; the focus panel swaps
// its big spend readout for these values.
export interface HistHover {
  line: "ctx" | "cost" | "floor";
  label: string; // small label above the number, e.g. "context"
  value: string; // the big number, preformatted
  time: string; // clock time of the hovered sample
  pct: number | null; // context share of target, for the gauge color
}

export interface Snapshot {
  sessions: Session[];
}

// Mirror of the Rust `baseline::Node`: one rectangle in the baseline treemap.
export interface BaselineNode {
  name: string;
  tokens: number;
  kind: "core" | "mcp" | "measured" | "deferred" | "messages";
  detail: string;
  /** Stable id of a Chat message block, for the click-to-open full-content panel. */
  gid?: number;
  /** ISO wall-clock of a Chat block/turn, for the "when" shown on sequential rects. */
  ts?: string;
  /** Cumulative spend (USD) for a browse-level tile (harness/project/session). */
  cost?: number;
  children: BaselineNode[];
}

// Mirror of the Rust `baseline::Report`.
export interface BaselineReport {
  ok: boolean;
  message: string;
  needsContext: boolean; // session has no /context snapshot yet
  total: number; // total context tokens at the snapshot
  max: number; // hard context window
  capturedAt: string; // ISO timestamp of the snapshot
  costRate: number; // estimated USD per context token (session model cache-read price)
  costIn: number; // USD per token, input rate (user prompts, tool results)
  costOut: number; // USD per token, output rate (agent text, thinking, tool calls)
  nodes: BaselineNode[];
}

// Mirror of the Rust `baseline::ContextSession`: a session that already has a
// `/context` snapshot, offered on the empty-state screen.
export interface ContextSession {
  id: string;
  title: string;
  project: string;
  subtitle: string; // last prompt, to disambiguate same-folder sessions
  capturedAt: string; // ISO timestamp of the newest /context run
}

// --- Cross-session browse (opt-in index; mirrors browse.rs) ---

// One harness/provider aggregate (Claude Code today).
export interface HarnessAgg {
  harness: string;
  label: string;
  projectCount: number;
  sessionCount: number;
  totalBytes: number;
  totalTokens: number; // summed over enriched sessions only
  totalCost: number;
}

// One project (directory) aggregate within a harness.
export interface ProjectAgg {
  harness: string;
  project: string;
  projectPath: string;
  sessionCount: number;
  totalBytes: number;
  totalTokens: number;
  totalCost: number;
  enrichedCount: number;
}

// One browsed session's cached metadata.
export interface SessionMeta {
  id: string;
  harness: string;
  project: string;
  projectPath: string;
  title: string;
  mtime: number; // unix ms
  sizeBytes: number;
  ctx: number | null;
  costUsd: number;
  model: string;
  turnCount: number;
  hasContextUsage: boolean;
  enriched: boolean; // Pass-2 has read its contents
}

// State of the browse index, for the gate + progress UI.
export interface BrowseStatus {
  enabled: boolean;
  dbExists: boolean;
  indexed: number;
  enriched: number;
  lastFullScan: number | null;
  scanning: boolean;
}

// Payload of the "browse-progress" event during a scan.
export interface BrowseProgress {
  phase: "index" | "enrich" | "done" | "canceled";
  done: number;
  total: number;
  current: string;
}

export interface SearchProgress {
  phase: "search" | "done" | "canceled";
  done: number;
  total: number;
  hits: number;
}

export interface SearchHit {
  id: string;
  title: string;
  project: string;
  projectPath: string;
  sizeBytes: number;
  mtime: number;
  firstMs: number; // first transcript turn (session start)
  lastMs: number; // last transcript turn (last activity)
  score: number;
  snippet: string;
}

// Mirror of the Rust `Config` (note the `N` key for max sessions).
export interface Config {
  N: number; // max sessions shown (candidate-pool ceiling; window height decides shown count)
  poll_seconds: number; // refresh interval
  target_tokens: number; // fallback gauge budget when the model is unknown
  follow_focus: boolean; // pin the session open in the Claude app to the top
  show_in_tray: boolean;
  show_in_taskbar: boolean;
  minimize_to_tray: boolean;
  close_to_tray: boolean;
  debug_logging: boolean;
  always_on_top: boolean;
  opacity: number; // window background opacity 0.3..1.0
  ui_scale: number; // UI zoom factor 0.5..3.0
  dim_hours: number; // age (h) at which a row reaches full dim; full <=1h, linear between
  theme: string; // color palette id; "auto" follows the OS (see THEMES in theme.ts)
  browse_enabled: boolean; // cross-session browse index built + shown (opt-in)
  show_statusbar: boolean; // bottom status bar: per-core CPU + memory usage
  show_prompt: boolean; // show each session's latest prompt under its title
  check_updates: boolean; // ask GitHub for a newer version at launch
  font_ui: string;
  font_num: string;
  font_mono: string;
  font_weight: number;
  size_ui: number;
  size_num: number;
  size_mono: number;
  font_sets: UserFontSet[]; // offer the Context Explorer (menu + clickable titles)
}

// Mirror of the Rust `stats::SysStats`, emitted on the "sysstats" event.
export interface SysStats {
  cpus: number[]; // per-core CPU usage, 0..100
  memUsed: number; // bytes
  memTotal: number; // bytes
  memPct: number; // used / total, 0..1
}

/** A font set the user saved. Flat, so it spreads straight onto the prefs. */
export interface UserFontSet {
  id: string;
  label: string;
  font_ui: string;
  font_num: string;
  font_mono: string;
  font_weight: number;
  size_ui: number;
  size_num: number;
  size_mono: number;
}
