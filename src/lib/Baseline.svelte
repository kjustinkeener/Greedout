<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import Icon from "./Icon.svelte";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { watchMaximized } from "./winChrome";
  import { gaugeColor, themeTick, applyTheme, type Theme } from "./theme";
  import Brand from "./Brand.svelte";
  import ScanProgress from "./ScanProgress.svelte";
  import { scanState, startScan as startCacheScan, cancelScan, onScanDone } from "./scanControl";
  import type {
    BaselineReport,
    BaselineNode,
    Session,
    HarnessAgg,
    ProjectAgg,
    SessionMeta,
    BrowseStatus,
    SearchHit,
    SearchProgress,
  } from "../types";

  // The session to analyze, seeded from the launch params. Runs in its own window
  // (closing closes it) and then FOLLOWS the selection: it listens to the backend
  // "sessions" stream and retargets whenever the focused/top session changes.
  let {
    id,
    title,
    initProject = "",
    initView = "",
  }: { id: string; title: string; initProject?: string; initView?: string } = $props();
  const onClose = () => getCurrentWindow().close();
  // Swap the maximize button to a restore glyph while the window is maximized.
  let maximized = $state(false);
  $effect(() => watchMaximized((m) => (maximized = m)));
  // Trace helper for the baseline feature: to the webview console for live
  // debugging, and to greedout.log (gated by the Debug logging setting) so the
  // existing log-to-file switch captures the UI side too.
  const dbg = (...a: unknown[]) => {
    console.debug("[baseline]", ...a);
    const msg = a
      .map((x) => (typeof x === "string" ? x : JSON.stringify(x)))
      .join(" ");
    invoke("baseline_log", { msg }).catch(() => {});
  };

  // Target = the launch param until the selection changes, then the override.
  let overrideId = $state<string | null>(null);
  let overrideTitle = $state<string | null>(null);
  let curId = $derived(overrideId ?? id);
  let curTitle = $derived(overrideTitle ?? title);
  let report = $state<BaselineReport | null>(null);
  // Estimated USD per context token for the current session (session model's
  // cache-read price). Drives the rough dollar figure on session-view rects.
  let costRate = $derived(report?.costRate ?? 0);
  let costIn = $derived(report?.costIn ?? 0);
  let costOut = $derived(report?.costOut ?? 0);
  // What sizes the rects in the session view: token count or estimated USD.
  let sizeMetric = $state<"tok" | "usd">("tok");
  let loadError = $state<string | null>(null);
  // Drill-in path: [] = top level; each entry is a node with children.
  let path = $state<BaselineNode[]>([]);
  // Names of tiles the user has hidden (✕), to exclude from the layout.
  let hidden = $state<Set<string>>(new Set());
  // The selected session's gauge budget (100% of the speedo scale).
  let target = $state(200000);
  let box = $state({ w: 520, h: 320 });
  let boxEl = $state<HTMLDivElement | undefined>();
  let hover = $state<Tile | null>(null);

  // When the user picks a session from the empty-state "recently seen" list, we
  // PIN to it: the auto-follow (which retargets to the app-focused session each
  // poll) would otherwise yank us straight back off the session they chose.
  let pinned = $state(false);

  // --- Cross-session browse (zoom out: Harness > Project > Session) ---
  // The window OPENS at the harness level (the all-projects graph); the user
  // drills project > session from there. Only Claude Code exists as a harness today.
  type Zoom = "root" | "harness" | "project" | "session";
  let zoom = $state<Zoom>("root");
  let curHarness = $state("claude-code");
  let harnesses = $state<HarnessAgg[]>([]);
  let curProject = $state<string | null>(null); // project of the session in view
  let selProject = $state<string | null>(null); // project being browsed at 'project' zoom
  let browse = $state<BrowseStatus | null>(null);
  let projects = $state<ProjectAgg[]>([]);
  let harnessAgg = $state<HarnessAgg | null>(null);
  let sessionRows = $state<SessionMeta[]>([]);
  let loadingBrowse = $state(false);
  let showEnable = $state(false); // the opt-in gate panel
  let deepScan = $state(false); // "deluxe" full-scan checkbox
  let scanning = $state(false);

  // --- Full-text search (chat text only) ---
  let searchQuery = $state("");
  let searchActive = $state(false); // results panel replaces the graph
  let searching = $state(false);
  let searchHits = $state<SearchHit[]>([]);
  let searchProg = $state<SearchProgress | null>(null);
  // Live search debounce. Trigram FTS needs a >=3-char term, so we only fire the
  // search live once the query reaches that floor; shorter queries can match only
  // on the slow disk path, so they wait for an explicit Enter.
  const LIVE_MIN = 3;
  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  // Adaptive debounce: never fire the next live search faster than the slowest
  // roundtrip seen so far, so a fast index stays snappy while a slow cache backs
  // off on its own. Ratchets up only (never down); clamped to a usable range.
  const DEBOUNCE_MIN = 120;
  const DEBOUNCE_MAX = 1500;
  let liveDebounce = $state(DEBOUNCE_MIN);
  let searchStart = 0;
  // Mutually-exclusive project filter over the results (null = all projects).
  let projectFilter = $state<string | null>(null);
  // Result ordering: "best" = relevance score (backend default), "recent" =
  // last-activity time. Hits stream in score-sorted; this re-sorts the shown set.
  let sortMode = $state<"best" | "recent">("best");
  // Projects present in the current results, with hit counts, most-hits first.
  let resultProjects = $derived.by(() => {
    const counts = new Map<string, number>();
    for (const h of searchHits) counts.set(h.project, (counts.get(h.project) ?? 0) + 1);
    return [...counts.entries()]
      .map(([project, count]) => ({ project, count }))
      .sort((a, b) => b.count - a.count || a.project.localeCompare(b.project));
  });
  // The active filter can go stale if its project drops out of a re-run's results.
  let shownHits = $derived.by(() => {
    const base =
      projectFilter && resultProjects.some((p) => p.project === projectFilter)
        ? searchHits.filter((h) => h.project === projectFilter)
        : searchHits;
    const arr = [...base];
    if (sortMode === "recent") arr.sort((a, b) => b.lastMs - a.lastMs || b.score - a.score);
    else arr.sort((a, b) => b.score - a.score || b.lastMs - a.lastMs);
    return arr;
  });

  function runSearch() {
    clearTimeout(searchTimer);
    const q = searchQuery.trim();
    if (!q) return;
    // The results panel is a body view, and the turn-detail view sits ahead of it
    // in the render chain: leave detail open and a search looks dead. Close it.
    detail = null;
    searchActive = true;
    searching = true;
    searchHits = [];
    searchProg = null;
    projectFilter = null;
    searchStart = performance.now();
    invoke("browse_search", { query: q }).catch((e) => {
      dbg("browse_search failed", e);
      searching = false;
    });
  }
  function cancelSearch() {
    invoke("browse_search_cancel").catch(() => {});
  }
  // Live search as the box changes: debounce, then fire once the query clears the
  // trigram floor. A superseding run bumps the backend's generation, so rapid
  // typing just cancels the previous run's emits (see run_search). When the query
  // drops back below the floor, tear the results down but keep what's typed.
  function onSearchInput() {
    clearTimeout(searchTimer);
    const q = searchQuery.trim();
    if (q.length < LIVE_MIN) {
      if (searchActive || searching) {
        if (searching) cancelSearch();
        searchActive = false;
        searching = false;
        searchHits = [];
        searchProg = null;
        projectFilter = null;
      }
      return;
    }
    searchTimer = setTimeout(runSearch, liveDebounce);
  }
  // Short harness label for a search-hit badge.
  function harnessLabel(h: string): string {
    return h === "codex" ? "Codex" : "Claude";
  }

  // Escape HTML, then bold the matched query terms in a snippet. Terms mirror the
  // backend: the whole query plus each word of length >= 2.
  function highlight(snippet: string): string {
    const esc = (s: string) =>
      s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
    const q = searchQuery.trim();
    const terms = [q, ...q.split(/\s+/)].filter((t) => t.length >= 2);
    if (!terms.length) return esc(snippet);
    const escRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    // Longest first so the full phrase wins over its individual words.
    const pat = terms.sort((a, b) => b.length - a.length).map(escRe).join("|");
    const re = new RegExp(pat, "gi");
    let out = "";
    let last = 0;
    let m: RegExpExecArray | null;
    while ((m = re.exec(snippet))) {
      if (m.index === re.lastIndex) re.lastIndex++;
      out += esc(snippet.slice(last, m.index)) + "<b>" + esc(m[0]) + "</b>";
      last = m.index + m[0].length;
    }
    return out + esc(snippet.slice(last));
  }
  function clearSearch() {
    clearTimeout(searchTimer);
    if (searching) cancelSearch();
    searchQuery = "";
    searchActive = false;
    searching = false;
    searchHits = [];
    searchProg = null;
    projectFilter = null;
  }
  // Open a search hit's session breakdown, pinning so auto-follow won't yank off it.
  function openHit(h: SearchHit) {
    pinned = true;
    // Same trap as selectSession: reopening the session already loaded leaves
    // curId unchanged, so the curId-keyed re-analyze effect never fires.
    const sameId = overrideId === h.id;
    overrideId = h.id;
    overrideTitle = `${h.title} · ${h.project}`;
    curProject = h.project;
    liveCtx = null;
    path = [];
    detail = null;
    loadError = null;
    zoom = "session";
    searchActive = false;
    if (sameId) analyze(h.id);
    else report = null;
    // Find which turn the first match sits in, so we can drill into the chat and
    // highlight that turn's tile once the breakdown loads.
    hlTurn = null;
    wantDrillToTurn = false;
    const q = searchQuery.trim();
    if (q) {
      invoke<number | null>("search_first_turn", { id: h.id, query: q })
        .then((t) => {
          if (t == null) return;
          hlTurn = t;
          wantDrillToTurn = true;
        })
        .catch((e) => dbg("search_first_turn failed", e));
    }
  }
  // Highlight a search hit's turn: `hlTurn` is the turn number (0 = Pre-turn),
  // `wantDrillToTurn` asks the loader-ready effect below to drill into the chat.
  let hlTurn = $state<number | null>(null);
  let wantDrillToTurn = $state(false);
  const turnName = (t: number) => (t === 0 ? "Pre-turn" : `Turn ${t}`);
  function isHlTurn(n: BaselineNode): boolean {
    return zoom === "session" && hlTurn != null && n.name === turnName(hlTurn);
  }
  // Once the chat breakdown is loaded, drill to the level that shows the turn
  // tiles: turns sit at the top level for an estimated session, else under the
  // "Messages" node. Runs once per hit (clears `wantDrillToTurn`).
  $effect(() => {
    if (!wantDrillToTurn || hlTurn == null) return;
    if (zoom !== "session" || !chatTree.length) return;
    if (estimated) {
      path = []; // turns are already the top level
    } else {
      const msgs = topNodes.find((n) => n.name === "Messages");
      if (!msgs || !msgs.children.length) return; // wait for chat kids to attach
      path = [msgs];
    }
    wantDrillToTurn = false;
  });
  // Stream hits + progress from the backend.
  $effect(() => {
    let un1: (() => void) | undefined;
    let un2: (() => void) | undefined;
    listen<SearchHit>("search-hit", (e) => {
      // Identity is (folder, session_id), NOT session_id alone: the same session id
      // can exist in two different project folders (e.g. a workspace copied/moved to
      // another path) and those are DISTINCT sessions. Dedup only on that compound
      // key, so a genuine re-emit collapses, but two folders' hits both survive.
      // (Keying the list below by id alone would throw `each_key_duplicate` and kill
      // the whole component's reactivity, freezing tiles AND the progress bar.)
      const key = (h: SearchHit) => `${h.projectPath} ${h.id}`;
      const byKey = new Map(searchHits.map((h) => [key(h), h]));
      const prev = byKey.get(key(e.payload));
      if (!prev || e.payload.score > prev.score) byKey.set(key(e.payload), e.payload);
      searchHits = [...byKey.values()].sort((a, b) => b.score - a.score);
    }).then((f) => (un1 = f));
    listen<SearchProgress>("search-progress", (e) => {
      searchProg = e.payload;
      if (e.payload.phase === "done" || e.payload.phase === "canceled") searching = false;
      // Ratchet the live debounce toward the slowest completed roundtrip. Only a
      // full "done" counts; a canceled run was cut short and isn't a real timing.
      if (e.payload.phase === "done") {
        const rt = performance.now() - searchStart;
        if (rt > liveDebounce) liveDebounce = Math.min(rt, DEBOUNCE_MAX);
      }
    }).then((f) => (un2 = f));
    return () => {
      un1?.();
      un2?.();
    };
  });

  // Land on the harness graph on first status load (or the enable gate if the
  // cross-session index isn't built yet). Guarded so later status refreshes
  // (after scans) don't re-navigate.
  let inited = $state(false);
  function loadBrowseStatus(openOnFirst = false) {
    invoke<BrowseStatus>("browse_status")
      .then((s) => {
        browse = s;
        scanning = s.scanning;
        if (openOnFirst && !inited) {
          inited = true;
          // "enabled" alone is NOT an index: after Clear cache the DB is gone but
          // the pref stays on, so require real rows. Opening the window with no
          // data ALWAYS auto-rebuilds (no enable gate): the shared cache is built
          // unconditionally by Daily Spend anyway, so gating here just showed an
          // empty graph. The browse-progress "done" handler repaints when it lands.
          const haveIndex = s.dbExists && s.indexed > 0;
          if (initView === "session") {
            // Land straight on the target session's breakdown.
            if (initProject) curProject = initProject;
            pinned = true;
            zoom = "session";
          } else if (initView === "project" && initProject) {
            if (haveIndex) openProject(initProject);
            else {
              openRoot();
              startScan(true);
            }
          } else {
            openRoot();
            if (!haveIndex) startScan(true);
          }
        }
      })
      .catch((e) => dbg("browse_status failed", e));
  }
  // Prime the status once and open the top-level graph.
  $effect(() => {
    loadBrowseStatus(true);
  });

  // Full rescan: resets the shared scanControl store and fires browse_scan. The
  // store's running flag flows back into `scanning` via the subscription below.
  function startScan(deep: boolean) {
    scanning = true;
    startCacheScan(deep);
  }
  function enableAndScan() {
    scanning = true;
    invoke("browse_enable", { deep: deepScan })
      .then(() => loadBrowseStatus())
      .catch((e) => {
        dbg("browse_enable failed", e);
        scanning = false;
      });
  }

  // Top level: every harness as a tile (only Claude Code today).
  function openRoot() {
    zoom = "root";
    showEnable = false;
    hidden = new Set();
    loadingBrowse = true;
    invoke<HarnessAgg[]>("browse_harnesses")
      .then((h) => (harnesses = h))
      .catch((e) => dbg("browse_harnesses failed", e))
      .finally(() => (loadingBrowse = false));
  }
  function openHarness(harness: string = curHarness) {
    curHarness = harness;
    zoom = "harness";
    showEnable = false;
    hidden = new Set();
    loadingBrowse = true;
    invoke<ProjectAgg[]>("browse_projects", { harness })
      .then((r) => (projects = r))
      .catch((e) => dbg("browse_projects failed", e))
      .finally(() => (loadingBrowse = false));
    invoke<HarnessAgg[]>("browse_harnesses")
      .then((h) => (harnessAgg = h.find((x) => x.harness === harness) ?? h[0] ?? null))
      .catch(() => {});
  }
  // Read this project's session tiles from the index (read-only, instant). Sizes
  // are always present; tokens/cost fill in as the background enrich lands.
  function refreshSessions(project: string) {
    return invoke<SessionMeta[]>("browse_sessions", { harness: curHarness, project })
      .then((r) => {
        sessionRows = r;
        dbg("browse_sessions", project, "->", r.length, "rows");
      })
      .catch((e) => dbg("browse_sessions failed", e));
  }
  // Throttled tile refresh during enrichment (see the progress effect).
  let lastRefresh = 0;
  let refreshTimer: ReturnType<typeof setTimeout> | undefined;
  function scheduleRefresh(project: string) {
    const now = Date.now();
    if (now - lastRefresh > 300) {
      lastRefresh = now;
      refreshSessions(project);
    } else {
      clearTimeout(refreshTimer);
      refreshTimer = setTimeout(() => {
        lastRefresh = Date.now();
        refreshSessions(project);
      }, 300);
    }
  }
  function openProject(project: string) {
    zoom = "project";
    selProject = project;
    showEnable = false;
    hidden = new Set();
    loadingBrowse = true;
    refreshSessions(project).finally(() => (loadingBrowse = false));
    // Kick off (or refresh) enrichment in the background: instant drill-in, and
    // the user can close this window while it keeps running.
    dbg("enrich project (bg)", project);
    scanning = true;
    invoke("browse_enrich_project", { harness: curHarness, project }).catch((e) => {
      dbg("browse_enrich_project failed", e);
      scanning = false;
    });
  }
  // Zoom out from the session view. Gates on the opt-in the first time.
  function zoomOut(level: "harness" | "project") {
    if (!browse?.enabled) {
      showEnable = true;
      return;
    }
    if (level === "project" && curProject) openProject(curProject);
    else openRoot();
  }
  // Drill from a browse tile down into a specific session's breakdown, pinning so
  // the auto-follow won't yank us off it.
  function selectSession(s: SessionMeta) {
    pinned = true;
    // Reopening the SAME session id leaves curId unchanged, so the curId-keyed
    // re-analyze effect won't fire, so analyze directly to avoid a stuck "Analyzing…".
    const sameId = overrideId === s.id;
    overrideId = s.id;
    overrideTitle = `${s.title} · ${s.project}`;
    curProject = s.project;
    liveCtx = null;
    path = [];
    loadError = null;
    zoom = "session";
    if (sameId) analyze(s.id);
    else report = null; // different id: the effect fires and re-analyzes
  }

  // Follow scan progress via the shared scanControl store (the bars/log render in
  // <ScanProgress> off the same store). Mirror running into our local `scanning`
  // and throttle tile refreshes while enrichment lands; onScanDone does the final
  // authoritative refresh of whatever level is showing.
  $effect(() => {
    const unSub = scanState.subscribe((s) => {
      if (!s.running) return;
      scanning = true;
      // Live-update the project tiles as rows are enriched. Coalesce: each
      // enriched row emits a progress event, and a naive refresh here re-reads
      // every row from SQLite AND re-lays-out the whole treemap per row (dozens
      // of times a second). Throttle to ~300ms; onScanDone does the final
      // authoritative refresh. `enrich.t0 && !enrich.end` = the enrich phase is
      // the one currently running.
      if (zoom === "project" && selProject && s.enrich.t0 && !s.enrich.end) {
        scheduleRefresh(selProject);
      }
    });
    const unDone = onScanDone(() => {
      scanning = false;
      loadBrowseStatus();
      // Refresh the current level READ-ONLY. Never re-open a project here: that
      // would re-trigger enrichment and loop.
      if (showEnable) openRoot();
      else if (zoom === "project" && selProject) refreshSessions(selProject);
      else if (zoom === "harness") openHarness();
      else if (zoom === "root") openRoot();
    });
    return () => {
      unSub();
      unDone();
    };
  });

  function fmtBytes(n: number): string {
    if (n >= 1e9) return (n / 1e9).toFixed(1) + " GB";
    if (n >= 1e6) return (n / 1e6).toFixed(1) + " MB";
    if (n >= 1e3) return (n / 1e3).toFixed(0) + " KB";
    return n + " B";
  }
  // Short local date; include the year only when it isn't the current one.
  function fmtDay(ms: number): string {
    if (!ms) return "";
    const d = new Date(ms);
    const sameYear = d.getFullYear() === new Date().getFullYear();
    return d.toLocaleDateString(undefined, {
      month: "short",
      day: "numeric",
      ...(sameYear ? {} : { year: "numeric" }),
    });
  }
  // "Aug 12 → Aug 24" for a session's activity span; a single date if same day.
  function fmtSpan(first: number, last: number): string {
    const a = fmtDay(first);
    const b = fmtDay(last);
    if (!a && !b) return "";
    if (!a || a === b) return b || a;
    return `${a} → ${b}`;
  }

  let rechecking = $state(false);
  // Outcome of the last manual "Check" press, shown under the button.
  let checkMsg = $state<{ ok: boolean; text: string } | null>(null);
  function recheck() {
    dbg("recheck (button)", curId?.slice(0, 8));
    checkMsg = null;
    rechecking = true;
    invoke<BaselineReport>("analyze_baseline", { id: curId })
      .then((r) => {
        dbg("recheck result", { ok: r.ok, needsContext: r.needsContext, total: r.total });
        report = r;
        loadError = r.ok ? null : r.message;
        if (!r.ok) {
          checkMsg = { ok: false, text: r.message };
        } else if (r.needsContext) {
          checkMsg = {
            ok: false,
            text: "No /context output found in this session yet. Run /context there, then check again.",
          };
        }
        // If a snapshot was found, the view switches to the treemap on its own.
      })
      .catch((e) => (checkMsg = { ok: false, text: String(e) }))
      .finally(() => (rechecking = false));
  }
  function analyze(target: string) {
    dbg("analyze", target?.slice(0, 8));
    rechecking = true;
    invoke<BaselineReport>("analyze_baseline", { id: target })
      .then((r) => {
        dbg("analyze result", target?.slice(0, 8), {
          ok: r.ok,
          needsContext: r.needsContext,
          total: r.total,
        });
        report = r;
        loadError = r.ok ? null : r.message;
      })
      .catch((e) => {
        dbg("analyze failed", e);
        loadError = String(e);
      })
      .finally(() => (rechecking = false));
  }
  // Re-analyze whenever the target session changes.
  $effect(() => {
    const target = curId;
    if (!target) return;
    analyze(target);
  });

  // Estimated Chat breakdown (Role > Turn > content), loaded per session and
  // rescaled to the live Chat total so drilling into the Chat tile shows it.
  let chatTree = $state<BaselineNode[]>([]);
  // The session id chatTree currently describes, so a stale/errored reload for a
  // different or same session never blanks a good tree.
  let chatTreeFor = $state<string | null>(null);
  function loadChat(target: string) {
    invoke<BaselineNode[]>("chat_breakdown", { id: target })
      .then((t) => {
        if (t.length > 0) {
          chatTree = t;
          chatTreeFor = target;
          dbg("loadChat ->", target?.slice(0, 8), "roles", t.length);
        } else if (chatTreeFor === target && chatTree.length > 0) {
          // Empty result for the SAME session we already have a tree for: almost
          // always a transient read (mid-write/compact), not a real emptying.
          // Keep the last good tree rather than collapsing to the dead-end state.
          dbg("loadChat empty (kept prior)", target?.slice(0, 8), "roles", chatTree.length);
        } else {
          // Genuinely nothing to show (new/other session with no transcript yet).
          chatTree = [];
          chatTreeFor = target;
          dbg("loadChat -> empty", target?.slice(0, 8));
        }
      })
      .catch((e) => {
        // A thrown call is transient (IPC/read/parse); never blank a good tree over it.
        dbg("loadChat failed (kept prior)", target?.slice(0, 8), "roles", chatTree.length, String(e));
      });
  }
  // Load the estimated Chat breakdown whenever we have a session, INCLUDING when
  // there's no /context snapshot yet: that estimate is what we fall back to so a
  // drilled-in session shows a breakdown instead of a dead-end check button.
  $effect(() => {
    if (report && curId) loadChat(curId);
  });
  // A session with no /context snapshot is shown from the transcript scan
  // (estimated tokens) rather than a dead end. Only when even that is empty (a
  // brand-new session with nothing to read) do we show the "run /context" note.
  let estimated = $derived(!!report?.needsContext);
  // Trace exactly why the "Nothing to scan" dead-end appears (vs the estimated
  // tree): it should only show when the transcript scan truly found nothing.
  $effect(() => {
    if (zoom === "session" && estimated && !topNodes.length) {
      dbg("empty-state shown", curId?.slice(0, 8), {
        needsContext: report?.needsContext,
        total: report?.total,
        chatRoles: chatTree.length,
        chatFor: chatTreeFor?.slice(0, 8),
      });
    }
  });
  // Multiply a node tree's tokens by a factor (used to fit the estimated Chat
  // breakdown to the live Chat token total).
  function scaleTree(nodes: BaselineNode[], f: number): BaselineNode[] {
    return nodes.map((n) => ({
      ...n,
      tokens: Math.round(n.tokens * f),
      children: scaleTree(n.children, f),
    }));
  }

  // While the empty state is up, watch the current session for new activity and
  // re-run the full analysis whenever it writes (its mtime advances). The poll
  // loop only tails the last 64KB, so a /context record that's scrolled past
  // that window is invisible to it -- but a whole-file re-check (same as the
  // button, or as switching away and back) always finds it.
  let lastMtime = $state(0);
  $effect(() => {
    if (!report?.needsContext) return;
    let un: (() => void) | undefined;
    listen<Session[]>("sessions", (e) => {
      const s = e.payload.find((x) => x.id === curId);
      if (!s) return;
      if (s.mtime !== lastMtime) {
        dbg("empty-state activity, re-analyzing", s.id.slice(0, 8), "mtime", s.mtime);
        lastMtime = s.mtime;
        analyze(curId);
      }
    }).then((f) => (un = f));
    return () => un?.();
  });

  // Follow the selection: the backend emits "sessions" each poll (<=1s), sorted
  // focused-then-newest, so payload[0] is the current selection. Retarget on change.
  $effect(() => {
    let un: (() => void) | undefined;
    listen<Session[]>("sessions", (e) => {
      const s = e.payload[0];
      if (!s) return;
      target = s.target || target; // keep the color scale on the selection's budget
      if (s.id === curId) {
        liveCtx = s.ctx; // keep the total/Messages live between /context snapshots
        curProject = s.project;
        return;
      }
      if (pinned || zoom !== "session") return; // pinned, or browsing zoomed-out
      dbg("follow retarget", curId?.slice(0, 8), "->", s.id.slice(0, 8));
      curId = s.id;
      curTitle = `${s.title} · ${s.project}`;
      curProject = s.project;
      liveCtx = s.ctx;
      path = [];
      report = null;
      loadError = null;
    }).then((f) => (un = f));
    return () => un?.();
  });

  // Follow live theme changes from Settings so the gauge colors re-apply here too.
  $effect(() => {
    let un: (() => void) | undefined;
    listen<Theme>("theme", (e) => applyTheme(e.payload)).then((f) => (un = f));
    return () => un?.();
  });

  // Retarget when the main window asks an already-open Explorer to jump to a
  // specific session or project (clicking a title/project in the gauge list).
  $effect(() => {
    let un: (() => void) | undefined;
    listen<{ id?: string; title?: string; project?: string; view?: string }>(
      "baseline-goto",
      (e) => {
        const p = e.payload;
        clearSearch();
        if (p.view === "project" && p.project) {
          curProject = p.project;
          if (browse?.enabled || (browse?.dbExists && (browse?.indexed ?? 0) > 0)) {
            openProject(p.project);
          } else {
            showEnable = true;
          }
          return;
        }
        // Default: a session view.
        pinned = true;
        overrideId = p.id ?? "";
        overrideTitle = p.title ?? "";
        curProject = p.project ?? null;
        liveCtx = null;
        path = [];
        report = null;
        loadError = null;
        showEnable = false;
        zoom = "session";
      },
    ).then((f) => (un = f));
    return () => un?.();
  });

  $effect(() => {
    if (!boxEl) return;
    const ro = new ResizeObserver(() => {
      if (boxEl) box = { w: boxEl.clientWidth, h: boxEl.clientHeight };
    });
    ro.observe(boxEl);
    return () => ro.disconnect();
  });

  // Live-patched top-level nodes: the /context snapshot's fixed parts (system,
  // tools, MCP, memory, skills) stay put, but Messages and the grand total track
  // the live context so the window keeps up between /context runs.
  let liveCtx = $state<number | null>(null);
  let topNodes = $derived.by<BaselineNode[]>(() => {
    // No /context snapshot: fall back to the estimated transcript breakdown
    // (Role > Turn > content), the "scan the session" view.
    if (estimated) return chatTree;
    const nodes = report?.nodes ?? [];
    // Chat (Messages) token total: the live figure when we have it (the focused
    // session), else the snapshot's own Messages total (a pinned/browsed session
    // never gets a live ctx). Either way we attach the estimated Role>Turn>content
    // children so the Chat tile is drillable.
    const snapMsgs = nodes.find((n) => n.name === "Messages")?.tokens ?? 0;
    const fixed = nodes.filter((n) => n.name !== "Messages").reduce((s, n) => s + n.tokens, 0);
    const msgs = liveCtx == null ? snapMsgs : Math.max(0, liveCtx - fixed);
    // Estimated Chat breakdown, rescaled so its leaves sum to the Chat total.
    const chatSum = chatTree.reduce((s, n) => s + n.tokens, 0);
    const kids = chatSum > 0 && msgs > 0 ? scaleTree(chatTree, msgs / chatSum) : [];
    const hasMsgs = nodes.some((n) => n.name === "Messages");
    const patched = nodes.map((n) =>
      n.name === "Messages" ? { ...n, tokens: msgs, children: kids } : n,
    );
    if (!hasMsgs && msgs > 0) {
      patched.push({
        name: "Messages",
        tokens: msgs,
        kind: "messages",
        detail: "the conversation so far (grows over time)",
        children: kids,
      });
    }
    return patched;
  });
  // Grand total shown: the live context if we have it, else the snapshot total.
  let displayTotal = $derived(
    estimated
      ? topNodes.reduce((s, n) => s + n.tokens, 0)
      : (liveCtx ?? report?.total ?? 0),
  );
  // Browse-level tiles (harness/project zoom): projects or sessions sized by
  // on-disk bytes. `detail` carries the id/path we need on click.
  let browseNodes = $derived.by<BaselineNode[]>(() => {
    if (zoom === "root") {
      return harnesses.map((h) => ({
        name: h.label,
        tokens: h.totalBytes,
        kind: "measured" as const,
        detail: h.harness,
        cost: h.totalCost,
        children: [],
      }));
    }
    if (zoom === "harness") {
      return projects.map((p) => ({
        name: p.project,
        tokens: p.totalBytes,
        kind: "measured" as const,
        detail: p.projectPath,
        cost: p.totalCost,
        children: [],
      }));
    }
    if (zoom === "project") {
      return sessionRows.map((s) => ({
        name: s.title,
        tokens: s.sizeBytes,
        kind: "measured" as const,
        detail: s.id,
        cost: s.costUsd,
        children: [],
      }));
    }
    return [];
  });

  // The nodes currently displayed: browse tiles when zoomed out, else the
  // session breakdown (children of the deepest drilled node, or the top level).
  let sessionBase = $derived.by<BaselineNode[]>(() =>
    path.length ? path[path.length - 1].children : topNodes,
  );
  let current = $derived.by<BaselineNode[]>(() => {
    if (zoom !== "session") return browseNodes.filter((n) => !hidden.has(nodeKey(n)));
    let base = sessionBase.filter((n) => !hidden.has(nodeKey(n)));
    if (typeFilter.size)
      base = base.filter((n) => {
        const t = blockType(n);
        return t == null || typeFilter.has(t); // group/non-chat nodes always shown
      });
    return base;
  });
  let currentTotal = $derived(current.reduce((s, n) => s + n.tokens, 0));
  // Total USD shown on the metric button: the session's role-priced estimate
  // inside a session, else the sum of measured spend across the browse tiles.
  let displayUsd = $derived(
    zoom === "session"
      ? topNodes.reduce((s, n) => s + nodeUsd(n), 0)
      : current.reduce((s, n) => s + (n.cost ?? 0), 0),
  );
  // Total of the current level in the ACTIVE size metric (tokens or USD), so a
  // tile's percentage matches whatever is sizing the rects.
  let currentSizeTotal = $derived(current.reduce((s, n) => s + sizeVal(n), 0));
  function tilePct(n: BaselineNode): string {
    const tot = currentSizeTotal || 1;
    const f = sizeVal(n) / tot;
    return (f * 100).toFixed(f < 0.1 ? 1 : 0) + "%";
  }

  // Multi-select block-type filter for the session view (User / Agent / Thinking /
  // Tool / Tool Result). Empty set = show all. Only leaf chat blocks have a type.
  const TYPE_ORDER = ["User", "Agent", "Thinking", "Tool", "Tool Result"];
  let typeFilter = $state<Set<string>>(new Set());
  function blockType(n: BaselineNode): string | null {
    if (n.children.length || n.kind !== "messages") return null;
    if (n.name === "Tool Result") return "Tool Result";
    if (n.name.startsWith("Tool:")) return "Tool";
    if (n.name === "User" || n.name === "Agent" || n.name === "Thinking") return n.name;
    return null;
  }
  // Block types present at the current session level, in canonical order, the
  // pills to offer. Empty (or a single type) means no useful filtering here.
  let availableTypes = $derived.by<string[]>(() => {
    if (zoom !== "session") return [];
    const present = new Set<string>();
    for (const n of sessionBase) {
      const t = blockType(n);
      if (t) present.add(t);
    }
    return TYPE_ORDER.filter((t) => present.has(t));
  });
  function toggleType(t: string) {
    const s = new Set(typeFilter);
    if (s.has(t)) s.delete(t);
    else s.add(t);
    typeFilter = s;
  }

  // Whether a browsed session tile already has a /context breakdown, so it can be
  // drilled straight into the baseline (vs. needing /context run there first).
  function hasBreakdown(n: BaselineNode): boolean {
    if (zoom !== "project") return false;
    return !!sessionRows.find((x) => x.id === n.detail)?.hasContextUsage;
  }

  // Plain-language explanation of a component, shown on tiles large enough to fit it.
  function about(n: BaselineNode): string {
    if (zoom === "root") {
      const h = harnesses.find((x) => x.harness === n.detail);
      return h ? `${h.projectCount} projects · ${h.sessionCount} sessions` : "";
    }
    if (zoom === "harness") return n.detail; // full project path
    if (zoom === "project") {
      const s = sessionRows.find((x) => x.id === n.detail);
      if (!s) return "";
      if (!s.enriched) return "size only, open to load tokens";
      const parts = [`${fmt(s.ctx ?? 0)} tok`, `${s.turnCount} turns`];
      if (s.model) parts.push(s.model);
      if (s.hasContextUsage) parts.push("/context ✓");
      return parts.join(" · ");
    }
    const name = n.name;
    if (name === "Memory & instructions")
      return "Your CLAUDE.md and memory files, injected into every turn.";
    if (name === "System prompt") return "Claude Code's base instructions.";
    if (name === "Built-in tools")
      return "Native tool schemas (Read, Edit, Bash, …). Deferred ones load on demand.";
    if (name === "MCP tools")
      return "Connected server tool schemas. Most load lazily; drill in for the per-server, per-tool split.";
    if (name === "Skills") return "Available skill descriptions loaded into context.";
    if (name === "Messages") return "The conversation so far; grows over time, unlike the rest.";
    return n.detail;
  }

  // A per-node key so hiding one tile doesn't hide its like-named siblings
  // (many nodes share a name, e.g. repeated "Tool: Edit" / "Result" blocks).
  function nodeKey(n: BaselineNode): string {
    return `${n.name}|${n.detail ?? ""}|${n.gid ?? ""}`;
  }
  // Hide a tile from the layout (FasterDB-style ✕); restore brings them all back.
  function hide(n: BaselineNode) {
    // Keep at least one tile on screen; hiding the last leaves an empty map.
    if (current.length <= 1) return;
    const h = new Set(hidden);
    h.add(nodeKey(n));
    hidden = h;
    if (hover && nodeKey(hover.node) === nodeKey(n)) hover = null;
  }
  function unhide(key: string) {
    const h = new Set(hidden);
    h.delete(key);
    hidden = h;
  }

  // Short local time for the snapshot label (the /context total is a snapshot,
  // not live; the speedo tracks the live total, so the two can differ).
  function snapTime(iso: string): string {
    const d = new Date(iso);
    if (isNaN(d.getTime())) return "";
    return d.toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
  }
  function fmt(n: number): string {
    return n >= 1000 ? (n / 1000).toFixed(n >= 10000 ? 0 : 1) + "k" : String(n);
  }
  // Spend readout for a browse-level tile. Cents under $1, else dollars.
  const fmtUsd = (n: number) => {
    if (n < 1) return `${(n * 100).toFixed(1)}¢`;
    const digits = n >= 10 ? 0 : 2; // whole dollars past $10
    return `$${n.toLocaleString(undefined, { minimumFractionDigits: digits, maximumFractionDigits: digits })}`;
  };
  // Short local wall-clock for a sequential rect's timestamp. Time-only if today,
  // else "MMM D, h:mm". Empty string when the node carries no ts.
  function fmtTime(ts?: string): string {
    if (!ts) return "";
    const d = new Date(ts);
    if (isNaN(d.getTime())) return "";
    const time = d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
    const now = new Date();
    const sameDay =
      d.getFullYear() === now.getFullYear() &&
      d.getMonth() === now.getMonth() &&
      d.getDate() === now.getDate();
    return sameDay ? time : `${d.toLocaleDateString([], { month: "short", day: "numeric" })}, ${time}`;
  }
  function pct(n: number): string {
    // Share of what's currently on screen (the visible tiles at this level,
    // excluding any the user has hidden), not of the gauge budget.
    const t = currentTotal || 1;
    return ((n / t) * 100).toFixed(n / t < 0.1 ? 1 : 0) + "%";
  }

  // Tile color = the speedo's own gauge scale, so it tracks the theme. `f` is the
  // tile's share of the gauge budget (tokens / target), exactly like the dial:
  // a tile equal to the whole 100% budget hits the redline end. A 135° gradient
  // between f just-below and just-above gives the same shaded look as the speedo.
  let tick = $state(0);
  $effect(() => themeTick.subscribe((n) => (tick = n)));
  // Color full-scale is FIXED at 50k tokens (a tile of 50k+ hits the redline end),
  // independent of the gauge target used for the % readout. When browsing (bytes,
  // not tokens), there's no meaningful absolute scale, so we color relative to the
  // largest tile at the current level instead.
  const COLOR_FULL = 50000;
  let colorMax = $derived(
    zoom === "session" ? COLOR_FULL : Math.max(1, ...current.map((n) => n.tokens)),
  );
  // Size readout on a tile: tokens for the breakdown, bytes when browsing.
  function tileSize(n: number): string {
    return zoom === "session" ? fmt(n) : fmtBytes(n);
  }
  // Estimated USD for a rect: measured spend at browse levels, else the session
  // view's per-token rate applied to the rect's tokens (rough, "even if est").
  function tileCost(n: BaselineNode): number | null {
    if (zoom !== "session") return n.cost ?? null;
    const u = nodeUsd(n);
    return u > 0 ? u : null;
  }
  // Estimated USD for a session node, priced by role like the main speedo: an
  // assistant-generated block (agent text / thinking / tool call) at the output
  // rate, a fed-in block (user prompt / tool result) at the input rate, and any
  // carried baseline tile (system, tools, memory, …) at the cache-read rate.
  // Groups (turns, Chat) sum their children.
  function nodeUsd(n: BaselineNode): number {
    if (zoom !== "session") return n.cost ?? 0;
    if (n.children.length) return n.children.reduce((s, c) => s + nodeUsd(c), 0);
    const t = blockType(n);
    if (t === "Agent" || t === "Thinking" || t === "Tool") return n.tokens * costOut;
    if (t === "User" || t === "Tool Result") return n.tokens * costIn;
    return n.tokens * costRate; // baseline/system tiles: resent as cache reads
  }
  // The value that sizes a tile: tokens/bytes, or measured/estimated spend when
  // the USD metric is picked. nodeUsd already returns the measured `cost` at
  // browse levels and the role-priced estimate inside a session, so USD sizing
  // now works at every level. Falls back to tokens when nothing here has a cost
  // yet (unenriched browse tiles) so tiles never collapse to zero area.
  function sizeVal(n: BaselineNode): number {
    return sizeMetric === "usd" && !usdEmpty ? nodeUsd(n) : n.tokens;
  }
  // True when USD sizing is selected but no tile at this level has a cost yet.
  let usdEmpty = $derived(sizeMetric === "usd" && !current.some((n) => nodeUsd(n) > 0));
  // Darken an "rgb(r,g,b)" string by a factor (1 = unchanged).
  function dim(rgb: string, factor: number): string {
    return rgb.replace(/\d+/g, (v) => String(Math.round(Number(v) * factor)));
  }
  // Footer legend bar, dimmed the same 50% as the tiles (0→50k across its width).
  let scaleBar = $derived.by(() => {
    void tick;
    const k = 0.5;
    return `linear-gradient(90deg, ${dim(gaugeColor(0), k)}, ${dim(gaugeColor(0.52), k)} 52%, ${dim(gaugeColor(1), k)})`;
  });
  // The two 135deg gradient stops for a tile, as [a, b]. Exposed to CSS as
  // --ta/--tb so :hover can brighten the stops without touching the box-shadow
  // inner glow.
  function tileFill(n: number): [string, string] {
    void tick; // recolor when the theme changes
    if (!n || n <= 0) return ["var(--muted)", "var(--muted)"];
    const f = n / colorMax;
    const b = 0.18; // bracket width → the two corners show distinct hues
    const k = 0.5; // knock 50% off tile brightness
    // Pin the hot corner at the redline and span the bracket BELOW it, so a tile
    // past 100% still fades red→orange instead of clamping to flat red.
    const hi = Math.min(f + b, 1);
    const lo = hi - 2 * b;
    return [dim(gaugeColor(lo), k), dim(gaugeColor(hi), k)];
  }
  // Tile stops. For leaf chat blocks in the session view, color by BLOCK TYPE
  // (the filter-chip's author accent) so a rect reads as User/Agent/Thinking/Tool
  // at a glance; everything else (turns, top-level, browse) keeps the gauge fill.
  function tileStops(n: BaselineNode): [string, string] {
    const t = zoom === "session" ? blockType(n) : null;
    if (!t) return tileFill(n.tokens);
    void tick; // recolor on theme switch
    const a = authorAccent(t);
    return [
      `color-mix(in srgb, ${a} 60%, #12171f)`,
      `color-mix(in srgb, ${a} 32%, #12171f)`,
    ];
  }
  // Dynamic label size: scales with the tile's WIDTH, 9px..34px.
  function labelFont(w: number, h: number): number {
    // Cap by height too, not just width: a wide-but-short tile must not get a
    // huge width-driven font that overflows vertically and clips the top lines.
    // Sum the line-height budget (in units of the returned base font) of exactly
    // the lines that will render at this tile size, mirroring the markup gates.
    const showMeta = h > 52;
    const showAbout = showMeta && w > 140 && h > 78;
    const showTime = showMeta && zoom === "session" && h > 44;
    let mult = 1.12; // name (0.8em * 1.4 line-height)
    if (showAbout) mult += 1.43; // 2 clamped lines * 0.42em * 1.3 + 0.8em margin
    if (showMeta) mult += 0.71; // size (0.62em * 1.15)
    if (showTime) mult += 0.58; // time (0.5em * 1.15)
    const byH = (h - 8) / mult; // minus 4px+4px vertical padding
    return Math.max(9, Math.min(w / 8, byH, 34));
  }

  function open(n: BaselineNode) {
    dbg("open", {
      zoom,
      name: n.name,
      kids: n.children.length,
      pathBefore: path.map((p) => p.name),
    });
    if (zoom === "root") {
      openHarness(n.detail);
      return;
    }
    if (zoom === "harness") {
      openProject(n.name);
      return;
    }
    if (zoom === "project") {
      // Match on id first; fall back to title so a click still lands if the row
      // list was swapped by a mid-enrichment refresh between paint and click.
      const s =
        sessionRows.find((x) => x.id === n.detail) ?? sessionRows.find((x) => x.title === n.name);
      if (s) selectSession(s);
      else dbg("open MISS: no session row", { detail: n.detail, name: n.name, rows: sessionRows.length });
      return;
    }
    if (n.children.length) {
      hlTurn = null; // a manual drill dismisses the search-hit highlight
      path = [...path, n];
      hidden = new Set(); // hidden tiles are per-level; reset on drill in
      typeFilter = new Set(); // type filter is per-level too
    } else if (n.kind === "messages" && n.gid != null) {
      openBlock(n); // a leaf message/tool block: show its full content
    }
  }

  // Full-window turn-detail view for a clicked Chat message/tool block. This is a
  // view-mode (fills the graph window), NOT an overlay: right-click / Esc returns
  // to the map. Metadata (turn #, ts, tokens, kind, gid, author) all comes off the
  // leaf Node + its parent Turn; only the full text is fetched via `chat_block`.
  let detail = $state<{
    label: string;
    text: string;
    loading: boolean;
    turn: number;
    ts: string;
    tokens: number;
    kind: string;
    gid: number;
  } | null>(null);
  // Turn number from a parent Turn node's name ("Turn 12" → 12, "Pre-turn" → 0).
  function parseTurn(name?: string): number {
    const m = (name ?? "").match(/(\d+)/);
    return m ? Number(m[1]) : 0;
  }
  function openBlock(n: BaselineNode, turnOverride?: number) {
    if (n.gid == null || !curId) return;
    // The parent Turn is the level we drilled into (last breadcrumb entry); when
    // stepping through the whole session the caller passes the turn explicitly.
    const parent = path[path.length - 1];
    const turn = turnOverride ?? parseTurn(parent?.name);
    dbg("detail open", { gid: n.gid, label: n.name, turn });
    detail = {
      label: n.name,
      text: "",
      loading: true,
      turn,
      ts: n.ts ?? "",
      tokens: n.tokens,
      kind: n.kind,
      gid: n.gid,
    };
    invoke<[string, string]>("chat_block", { id: curId, gid: n.gid })
      .then(([label, text]) => {
        if (detail) detail = { ...detail, label: label || n.name, text, loading: false };
      })
      .catch((e) => {
        if (detail) detail = { ...detail, text: String(e), loading: false };
      });
  }
  // Explicit author label + a theme-color accent that maps sensibly:
  // User=accent/live, Agent=fg, Thinking=muted, Tool*/Result=gauge-mid.
  function authorAccent(label: string): string {
    if (label === "User") return "var(--live)";
    if (label === "Agent") return "var(--fg)";
    if (label === "Thinking") return "var(--muted)";
    return "var(--g1)"; // "Tool: X" and "Tool Result"
  }
  // Every leaf chat block across the whole session, gid-ordered, each tagged with
  // its turn, the sequence the detail view's prev/next buttons walk.
  let flatBlocks = $derived.by<{ node: BaselineNode; turn: number }[]>(() => {
    const out: { node: BaselineNode; turn: number }[] = [];
    for (const t of chatTree) {
      const tn = parseTurn(t.name);
      for (const leaf of t.children) if (leaf.gid != null) out.push({ node: leaf, turn: tn });
    }
    out.sort((a, b) => (a.node.gid ?? 0) - (b.node.gid ?? 0));
    return out;
  });
  let detailIndex = $derived(
    detail ? flatBlocks.findIndex((f) => f.node.gid === detail!.gid) : -1,
  );
  // Step to the previous/next block in the session (any type), from the detail view.
  function stepDetail(delta: number) {
    if (detailIndex < 0) return;
    const ni = detailIndex + delta;
    if (ni < 0 || ni >= flatBlocks.length) return;
    openBlock(flatBlocks[ni].node, flatBlocks[ni].turn);
  }
  function crumb(i: number) {
    hlTurn = null; // navigating away dismisses the search-hit highlight
    path = path.slice(0, i);
    hidden = new Set(); // and on drill out
    typeFilter = new Set();
  }
  // One step back up, for right-click anywhere on the map. Walks the full ladder:
  // deeper breakdown levels > session top > project browse > harness > root.
  function zoomOutOne() {
    if (zoom === "session") {
      if (path.length) crumb(path.length - 1);
      else if (curProject) zoomOut("project");
      else zoomOut("harness");
    } else if (zoom === "project") openHarness();
    else if (zoom === "harness") openRoot();
    // zoom === "root": already at the top, nothing above.
  }

  // Session-view breadcrumb, capped to 4 visible levels. Full ladder is
  // All › project › session › Chat › Turn N › block; when deeper than 4 the
  // middle collapses to an ellipsis, always keeping All + the last 3.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let shownCrumbs = $derived.by<any[]>(() => {
    const all: any[] = [];
    all.push({
      key: "all",
      icon: "expand",
      label: "All",
      act: () => zoomOut("harness"),
      title: "Browse all projects",
    });
    if (curProject)
      all.push({
        key: "proj",
        label: curProject,
        act: () => zoomOut("project"),
        title: "Other sessions in this project",
      });
    const sess = (curTitle || "").split(" · ")[0] || "Session";
    all.push({ key: "sess", label: sess, act: () => crumb(0), title: curTitle });
    path.forEach((p, i) =>
      all.push({
        key: "p" + i,
        label: p.name === "Messages" ? "Chat" : p.name,
        act: () => crumb(i + 1),
        title: p.name,
      }),
    );
    const MAX = 4;
    if (all.length <= MAX) {
      return all.map((c, i) => ({ ...c, first: i === 0, last: i === all.length - 1 }));
    }
    const tail = all.slice(all.length - (MAX - 1));
    return [
      { ...all[0], first: true, last: false },
      { key: "gap", gap: true, hidden: all.length - MAX },
      ...tail.map((c, i) => ({ ...c, first: false, last: i === tail.length - 1 })),
    ];
  });

  // --- Squarified treemap layout ---
  interface Tile {
    node: BaselineNode;
    x: number;
    y: number;
    w: number;
    h: number;
  }

  function worst(
    row: BaselineNode[],
    len: number,
    scale: number,
    wOf: (n: BaselineNode) => number,
  ): number {
    if (len === 0 || row.length === 0) return Infinity;
    let min = Infinity,
      max = 0,
      sum = 0;
    for (const n of row) {
      const a = wOf(n) * scale;
      sum += a;
      if (a < min) min = a;
      if (a > max) max = a;
    }
    const s2 = sum * sum;
    const l2 = len * len;
    return Math.max((l2 * max) / s2, s2 / (l2 * min));
  }

  // Squarified treemap. `ordered` keeps the input sequence (no size sort) for the
  // single-session view, so blocks read in conversation order while tiles stay
  // proportional-area and roughly square (strips grow along the shorter side).
  function squarify(
    nodes: BaselineNode[],
    W: number,
    H: number,
    ordered = false,
    wOf: (n: BaselineNode) => number = (n) => n.tokens,
  ): Tile[] {
    const kept = nodes.filter((n) => wOf(n) > 0);
    const items = ordered ? kept : kept.slice().sort((a, b) => wOf(b) - wOf(a));
    const total = items.reduce((s, n) => s + wOf(n), 0);
    if (!total || W <= 0 || H <= 0) return [];
    const scale = (W * H) / total;
    const tiles: Tile[] = [];
    let x = 0,
      y = 0,
      w = W,
      h = H;
    let i = 0;
    while (i < items.length) {
      const row: BaselineNode[] = [];
      const vertical = w >= h; // lay the row along the shorter side
      const side = vertical ? h : w;
      let best = Infinity;
      let j = i;
      // Grow the row while it improves the aspect ratio.
      while (j < items.length) {
        const trial = [...row, items[j]];
        const wq = worst(trial, side, scale, wOf);
        if (row.length && wq > best) break;
        row.push(items[j]);
        best = wq;
        j++;
      }
      const rowSum = row.reduce((s, n) => s + wOf(n), 0) * scale;
      const thick = side > 0 ? rowSum / side : 0;
      let off = 0;
      for (const n of row) {
        const len = ((wOf(n) * scale) / (thick || 1));
        if (vertical) {
          tiles.push({ node: n, x, y: y + off, w: thick, h: len });
        } else {
          tiles.push({ node: n, x: x + off, y, w: len, h: thick });
        }
        off += len;
      }
      if (vertical) {
        x += thick;
        w -= thick;
      } else {
        y += thick;
        h -= thick;
      }
      i = j;
    }
    return tiles;
  }

  let tiles = $derived(
    zoom === "session"
      ? squarify(current, box.w, box.h, true, sizeVal) // keep conversation order
      : squarify(current, box.w, box.h, false, sizeVal),
  );
  // Surface any uncaught webview error into greedout.log (gated by the Debug
  // setting via dbg). A thrown error inside an effect/render silently disables
  // Svelte's scheduler for this window, so without this a crash looks like a
  // dead UI with no trace. Cheap to keep; leave it in.
  $effect(() => {
    const onErr = (e: ErrorEvent) => dbg("WINDOW ERROR", String(e.message), String(e.error?.stack ?? ""));
    const onRej = (e: PromiseRejectionEvent) => dbg("UNHANDLED REJECTION", String(e.reason));
    window.addEventListener("error", onErr);
    window.addEventListener("unhandledrejection", onRej);
    return () => {
      window.removeEventListener("error", onErr);
      window.removeEventListener("unhandledrejection", onRej);
    };
  });
  // Trace what the treemap is actually asked to render, to catch "rows arrived but
  // nothing shows" (zoom mismatch, empty current, zero box, or squarify wipeout).
  $effect(() => {
    dbg("render", { zoom, cur: current.length, tiles: tiles.length, pathNames: path.map((p) => p.name), box, loadingBrowse, scanning });
  });
</script>

{#snippet mapArea()}
  <div
    class="map"
    bind:this={boxEl}
    role="presentation"
    onmouseleave={() => (hover = null)}
    oncontextmenu={(e) => {
      e.preventDefault();
      zoomOutOne();
    }}
  >
    {#each tiles as t, i (t.node.name + "|" + t.node.detail + "|" + (t.node.gid ?? "x") + "|" + i)}
      <div
        class="tile"
        class:drill={zoom !== "session" || t.node.children.length > 0}
        class:est={t.node.kind === "deferred"}
        class:hl={isHlTurn(t.node)}
        role="button"
        tabindex="0"
        style="left:{t.x + 1}px; top:{t.y + 1}px; width:{Math.max(0, t.w - 2)}px; height:{Math.max(
          0,
          t.h - 2,
        )}px; --ta:{tileStops(t.node)[0]}; --tb:{tileStops(t.node)[1]}; --sh:{Math.max(
          0,
          Math.min(t.w - 2, t.h - 2),
        )}px;"
        onclick={() => open(t.node)}
        oncontextmenu={(e) => {
          e.preventDefault();
          e.stopPropagation();
          zoomOutOne();
        }}
        onkeydown={(e) => (e.key === "Enter" || e.key === " ") && open(t.node)}
        onmouseenter={() => (hover = t)}
        onmouseleave={() => (hover === t ? (hover = null) : null)}
      >
        {#if t.w > 50 && t.h > 26}
          {@const meta = t.h > 52}
          <span class="tm-label" style="font-size:{labelFont(t.w, t.h)}px;">
            <span class="tm-name" style="font-size:{labelFont(t.w, t.h) * 0.8}px;"
              >{t.node.name === "Messages" ? "Chat" : t.node.name}</span
            >
            {#if meta}
              {#if t.w > 140 && t.h > 78}
                <span class="tm-about" style="font-size:{Math.max(10, labelFont(t.w, t.h) * 0.42)}px;"
                  >{about(t.node)}</span
                >
              {/if}
              <span class="tm-size" style="font-size:{Math.max(10, labelFont(t.w, t.h) * 0.62)}px;"
                >{#if sizeMetric === "tok" && (tileCost(t.node) ?? 0) > 0 && t.w > 56 && t.h > 44}<span
                    class="tm-cost">{fmtUsd(tileCost(t.node) ?? 0)}</span
                  >{" · "}{/if}{sizeMetric === "usd" && !usdEmpty
                  ? fmtUsd(nodeUsd(t.node))
                  : tileSize(t.node.tokens)} · {tilePct(t.node)}</span
              >
              {#if zoom === "session" && fmtTime(t.node.ts) && t.w > 64 && t.h > 44}
                <span class="tm-time" style="font-size:{Math.max(9, labelFont(t.w, t.h) * 0.5)}px;"
                  >{fmtTime(t.node.ts)}</span
                >
              {/if}
            {/if}
          </span>
        {/if}
        {#if current.length > 1 && t.w > 42 && t.h > 24}
          <button
            class="tm-x"
            aria-label="Hide {t.node.name}"
            onclick={(e) => {
              e.stopPropagation();
              hide(t.node);
            }}><Icon name="x" size={11} width={2.4} /></button
          >
        {/if}
        {#if hasBreakdown(t.node) && t.w > 30 && t.h > 20}
          <span class="tm-ctx" title="Has a /context breakdown, drill in for the baseline">
            <Icon name="layout" size={10} width={2.4} />
          </span>
        {/if}
      </div>
    {/each}
  </div>
{/snippet}

{#snippet scanBar()}
  <ScanProgress oncancel={cancelScan} />
{/snippet}

<svelte:window
  onkeydown={(e) => {
    // Never steal arrow keys from a focused text field (the search box).
    const typing = (e.target as HTMLElement | null)?.tagName === "INPUT";
    if (e.key === "Escape") detail ? (detail = null) : onClose();
    else if (typing) return;
    else if (detail && e.key === "ArrowLeft") stepDetail(-1);
    else if (detail && e.key === "ArrowRight") stepDetail(1);
  }}
  oncontextmenu={(e) => {
    // While the turn-detail view fills the window, a right-click anywhere returns
    // to the map (and is swallowed so it doesn't also trigger the map's zoom-out).
    if (detail) {
      e.preventDefault();
      detail = null;
    }
  }}
/>
<div class="win">
  <div class="modal">
    <header data-tauri-drag-region>
      <Brand size={16} font={15} />
      <div class="titles" data-tauri-drag-region>
        <span class="sub" data-tauri-drag-region>Context Explorer</span>
      </div>
      <div class="search">
        <span class="sicon"><Icon name="search" size={13} /></span>
        <input
          class="sinput"
          type="text"
          placeholder="Search chat text…"
          bind:value={searchQuery}
          oninput={onSearchInput}
          onkeydown={(e) => {
            if (e.key === "Enter") runSearch();
            else if (e.key === "Escape" && (searchQuery || searchActive)) {
              e.stopPropagation();
              clearSearch();
            }
          }}
        />
        {#if searchActive && !searching}
          <button class="sx" aria-label="Search again" title="Search again" onclick={runSearch}
            ><Icon name="refresh" size={11} /></button
          >
        {/if}
        {#if searchQuery || searchActive}
          <button class="sx" aria-label="Clear search" onclick={clearSearch}><Icon name="x" size={11} /></button>
        {/if}
      </div>
      <!-- The OS close button is gone with decorations off, so the header carries
           its own. -->
      <button class="hx" onclick={() => getCurrentWindow().minimize()} aria-label="Minimize"
        ><Icon name="minus" size={14} /></button
      >
      <button
        class="hx"
        onclick={() => getCurrentWindow().toggleMaximize()}
        aria-label={maximized ? "Restore" : "Maximize"}
        ><Icon name={maximized ? "restore" : "maximize"} size={12} /></button
      >
      <button class="hx" onclick={onClose} aria-label="Close"><Icon name="x" size={14} /></button>
    </header>

    {#if detail}
      <div class="detail">
        <div class="dhead" style="--accent:{authorAccent(detail.label)};">
          <button class="crumb back dback" onclick={() => (detail = null)} title="Back to map (Esc / right-click)"
          >
            <Icon name="chevron-left" size={11} />Map</button
          >
          <span class="dauthor">{detail.label}</span>
          {#if detail.turn > 0}<span class="dturn">Turn {detail.turn}</span>{/if}
          <span class="dnav">
            <button
              class="crumb back dstep"
              disabled={detailIndex <= 0}
              onclick={() => stepDetail(-1)}
              title="Previous block (←)"
            >
              <Icon name="chevron-left" size={11} />Prev</button
            >
            {#if detailIndex >= 0}<span class="dpos">{detailIndex + 1} / {flatBlocks.length}</span>{/if}
            <button
              class="crumb back dstep"
              disabled={detailIndex < 0 || detailIndex >= flatBlocks.length - 1}
              onclick={() => stepDetail(1)}
              title="Next block (→)"
            >
              Next<Icon name="chevron-right" size={11} /></button
            >
          </span>
          <button class="dx" aria-label="Close" title="Back to map (Esc / right-click)" onclick={() => (detail = null)}
          >
            <Icon name="x" size={13} /></button
          >
        </div>
        <div class="dmeta">
          <span class="mrow"><span class="mk">author</span><span class="mv">{detail.label}</span></span>
          <span class="mrow"><span class="mk">turn</span><span class="mv"
              >{detail.turn > 0 ? `#${detail.turn}` : "pre-turn"}</span
            ></span
          >
          <span class="mrow"><span class="mk">when</span><span class="mv"
              >{detail.ts ? snapTime(detail.ts) : "-"}</span
            ></span
          >
          <span class="mrow"><span class="mk">est. tokens</span><span class="mv">≈{fmt(detail.tokens)}</span></span>
          <span class="mrow"><span class="mk">kind</span><span class="mv">{detail.kind}</span></span>
          <span class="mrow"><span class="mk">gid</span><span class="mv">{detail.gid}</span></span>
        </div>
        {#if detail.loading}
          <div class="msg">Loading…</div>
        {:else}
          <pre class="dbody">{detail.text}</pre>
        {/if}
      </div>
    {:else if searchActive}
      <div class="bar">
        <span class="sresults"
          >{shownHits.length} match{shownHits.length === 1 ? "" : "es"}
          {#if searching}<span class="asof">· scanning {searchProg?.done ?? 0}/{searchProg?.total ?? 0}</span>{/if}
        </span>
        {#if searchHits.length > 1}
          <span class="sortsel">
            <button
              class="ppill"
              class:on={sortMode === "best"}
              onclick={() => (sortMode = "best")}
              title="Most relevant first">Best</button
            >
            <button
              class="ppill"
              class:on={sortMode === "recent"}
              onclick={() => (sortMode = "recent")}
              title="Most recent first">Recent</button
            >
          </span>
        {/if}
        {#if searching}
          <span class="grand"><button class="crumb back" onclick={cancelSearch}>Cancel</button></span>
        {/if}
      </div>
      {#if resultProjects.length > 1}
        <div class="bar pfilterbar">
          <span class="pfilters">
            <button
              class="ppill"
              class:on={projectFilter === null}
              onclick={() => (projectFilter = null)}
              title="Show all projects">All</button
            >
            {#each resultProjects as p (p.project)}
              <button
                class="ppill"
                class:on={projectFilter === p.project}
                onclick={() => (projectFilter = projectFilter === p.project ? null : p.project)}
                title="Only {p.project}">{p.project} <span class="pn">{p.count}</span></button
              >
            {/each}
          </span>
        </div>
      {/if}
      {#if searching}
        <div class="scanwrap">
          <div class="ptrack">
            <div
              class="pfill"
              style="width:{searchProg && searchProg.total
                ? (searchProg.done / searchProg.total) * 100
                : 0}%"
            ></div>
          </div>
        </div>
      {/if}
      <div class="results">
        {#if !searchHits.length}
          <div class="msg">
            {searching ? "Searching…" : `No sessions match “${searchQuery}”.`}
          </div>
        {:else}
          {#each shownHits as h (h.projectPath + "|" + h.id)}
            <button class="hit" onclick={() => openHit(h)}>
              <div class="hrow">
                <span class="hbadge" class:codex={h.harness === "codex"}>{harnessLabel(h.harness)}</span>
                <span class="htitle">{h.title}</span>
                <span class="hproj">{h.project}</span>
                <span class="hdates" title="First → last activity">{fmtSpan(h.firstMs, h.lastMs)}</span>
                <span class="hsize">{fmtBytes(h.sizeBytes)}</span>
              </div>
              <div class="hsnip">{@html highlight(h.snippet)}</div>
            </button>
          {/each}
        {/if}
      </div>
    {:else if showEnable}
      <div class="empty">
        <div class="etitle">Browse context across every session</div>
        <p class="ebody">
          Zoom out from this one session to all your projects. Greedout builds a local index of
          every Claude Code transcript (in <code>%LOCALAPPDATA%\Greedout\cache.sqlite</code>) so it can
          size projects and sessions. The first pass just reads file sizes and is quick; token and
          cost details load as you drill in.
        </p>
        <label class="deepopt">
          <input type="checkbox" bind:checked={deepScan} />
          Full deep scan now: read every transcript up front (slower, but tokens &amp; cost ready everywhere)
        </label>
        {#if scanning}
          {@render scanBar()}
        {:else}
          <button class="echk" onclick={enableAndScan}>Enable &amp; scan</button>
          <button class="crumb back" onclick={() => (showEnable = false)}>Cancel</button>
        {/if}
      </div>
    {:else if zoom !== "session"}
      <div class="bar">
        <nav class="crumbs">
          <button class="crumb home" onclick={openRoot} title="All harnesses" aria-label="Home">
            <Icon name="home" size={14} width={1.8} />
          </button>
          {#if zoom !== "root"}
            <span class="sep">›</span>
            <button class="crumb" class:cur={zoom === "harness"} onclick={() => openHarness()}
              >{harnessAgg?.label ?? "Claude Code"}</button
            >
          {/if}
          {#if zoom === "project"}
            <span class="sep">›</span>
            <button class="crumb cur">{selProject}</button>
          {/if}
        </nav>
        <span class="grand">
          <span class="bcount">
            {#if zoom === "root"}
              {harnesses.length} harnesses
            {:else if zoom === "harness"}
              {projects.length} projects
            {:else}
              {sessionRows.length} sessions
            {/if}
          </span>
          <span class="metricsel" role="group" aria-label="Size tiles by">
            <button
              class="mbtn"
              class:on={sizeMetric !== "usd"}
              onclick={() => (sizeMetric = "tok")}
              title="Size tiles by on-disk bytes">{fmtBytes(currentTotal)}</button
            >
            <button
              class="mbtn"
              class:on={sizeMetric === "usd"}
              onclick={() => (sizeMetric = "usd")}
              title="Size tiles by measured spend">{fmtUsd(displayUsd)}</button
            >
          </span>
          <button class="crumb back rescan" onclick={() => startScan(true)} disabled={scanning}
          >
            {#if !scanning}<Icon name="refresh" size={11} />{/if}{scanning
              ? "scanning…"
              : "rescan"}</button
          >
        </span>
      </div>

      {#if scanning}
        <div class="scanwrap">{@render scanBar()}</div>
      {/if}
      {#if loadingBrowse && !current.length}
        <div class="msg">Loading…</div>
      {:else if !current.length}
        {#if !scanning}
          <div class="msg">Nothing indexed yet. Try ↻ rescan.</div>
        {/if}
      {:else}
        {@render mapArea()}
      {/if}

      {#if hidden.size}
        <div class="hidden-bar">
          <span class="hlabel">hidden</span>
          {#each [...hidden] as key (key)}
            <button class="pill" onclick={() => unhide(key)} title="Show again"
              >{key.split("|")[0]} <span class="px"><Icon name="x" size={9} width={2.4} /></span></button
            >
          {/each}
          <button class="crumb back tclear" onclick={() => (hidden = new Set())}>clear</button>
        </div>
      {/if}

      <footer>
        <span class="scale">
          <span class="slabel">0</span>
          <span class="sbar" style="background:{scaleBar}"></span>
          <span class="slabel">{fmtBytes(colorMax)}</span>
        </span>
        <span class="legend">on-disk size</span>
        {#if hover}
          <span class="hover-info">
            <span class="hname">{hover.node.name}</span>
            <span class="hmeta">{fmtBytes(hover.node.tokens)} · {pct(hover.node.tokens)} of shown</span>
          </span>
        {/if}
      </footer>
    {:else if loadError}
      <div class="msg err">{loadError}</div>
    {:else if !report}
      <div class="msg">Analyzing…</div>
    {:else if estimated && !topNodes.length}
      <div class="bar">
        <nav class="crumbs">
          <button class="crumb back" onclick={() => zoomOut("harness")} title="Browse all projects"
          >
            <Icon name="expand" size={11} />All</button
          >
          {#if curProject}
            <span class="sep">›</span>
            <button
              class="crumb"
              onclick={() => zoomOut("project")}
              title="Other sessions in this project">{curProject}</button
            >
          {/if}
        </nav>
      </div>
      <div class="empty">
        <div class="etitle">Nothing to scan in this session yet</div>
        <p class="ebody">
          This session has no messages to break down and no <code>/context</code> snapshot. Run
          <code>/context</code> in it for the exact baseline, then check again, or zoom back out above.
        </p>
        <button class="echk" onclick={recheck} disabled={rechecking}>
          {rechecking ? "Checking…" : "Check Session for /context"}
        </button>
        {#if checkMsg}
          <div class="cmsg" class:bad={!checkMsg.ok}>{checkMsg.text}</div>
        {/if}
      </div>
    {:else}
      <div class="bar">
        <nav class="crumbs">
          {#each shownCrumbs as c (c.key)}
            {#if c.gap}
              <span class="sep">›</span>
              <span class="crumb ellip" title="{c.hidden} more level(s)">…</span>
            {:else}
              {#if !c.first}<span class="sep">{c.dot ? "·" : "›"}</span>{/if}
              <button
                class="crumb"
                class:back={c.first}
                class:cur={c.last}
                onclick={c.act}
                title={c.title}
              >
                {#if c.icon}<Icon name={c.icon} size={11} />{/if}{c.label}</button
              >
            {/if}
          {/each}
        </nav>
        <span class="grand">
          <span class="metricsel" role="group" aria-label="Size rects by">
            <!-- tok is a point-in-time context reading; summing it across
                 sessions/projects is meaningless, so only the session level
                 shows a tok total. Browse levels get a bytes button in their
                 own bar. Cost is additive and stays at every level. -->
            <button
              class="mbtn"
              class:on={sizeMetric === "tok"}
              onclick={() => (sizeMetric = "tok")}
              title="Size tiles by tokens">{fmt(displayTotal)} tok</button
            >
            <button
              class="mbtn"
              class:on={sizeMetric === "usd"}
              onclick={() => (sizeMetric = "usd")}
              title="Size tiles by estimated cost">{fmtUsd(displayUsd)}</button
            >
          </span>{#if estimated}{:else if report.capturedAt}<span class="asof"
              >{liveCtx != null ? "live · parts" : "as of"}
              {snapTime(report.capturedAt)}</span
            >{/if}</span
        >
      </div>

      {#if availableTypes.length > 1}
        <div class="typebar">
          <span class="hlabel">type</span>
          {#each availableTypes as t (t)}
            <button
              class="tpill"
              class:on={typeFilter.has(t)}
              style="--accent:{authorAccent(t)};"
              onclick={() => toggleType(t)}
              title="Filter to {t} blocks">{t}</button
            >
          {/each}
          {#if typeFilter.size}
            <button class="crumb back tclear" onclick={() => (typeFilter = new Set())}>clear</button>
          {/if}
        </div>
      {/if}

      {@render mapArea()}

      {#if hidden.size}
        <div class="hidden-bar">
          <span class="hlabel">hidden</span>
          {#each [...hidden] as key (key)}
            <button class="pill" onclick={() => unhide(key)} title="Show again"
              >{key.split("|")[0]} <span class="px"><Icon name="x" size={9} width={2.4} /></span></button
            >
          {/each}
          <button class="crumb back tclear" onclick={() => (hidden = new Set())}>clear</button>
        </div>
      {/if}

      <footer>
        <span class="scale">
          <span class="slabel">0</span>
          <span class="sbar" style="background:{scaleBar}"></span>
          <span class="slabel">50k</span>
        </span>
        <span class="legend">{estimated ? "estimated from transcript" : "from /context"}</span>
        {#if estimated && checkMsg}
          <span class="cmsg inline" class:bad={!checkMsg.ok}>{checkMsg.text}</span>
        {/if}
        {#if hover}
          <span class="hover-info">
            <span class="hname">{hover.node.name === "Messages" ? "Chat" : hover.node.name}</span>
            <span class="hmeta">{fmt(hover.node.tokens)} tok · {pct(hover.node.tokens)} of shown</span>
            {#if hover.node.detail && hover.node.detail !== hover.node.kind}
              <span class="hpreview" title={hover.node.detail}>{hover.node.detail}</span>
            {/if}
          </span>
        {/if}
      </footer>
    {/if}

  </div>
</div>

<style>
  .win {
    position: fixed;
    inset: 0;
    /* The Context Explorer is a reading view: keep it fully opaque so text stays
       legible even when the opacity slider makes the main window see-through. */
    background: rgb(var(--bg-rgb));
  }
  .modal {
    width: 100%;
    height: 100%;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    overflow: hidden;
  }
  header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    border-bottom: 1px solid var(--edge);
  }
  .titles {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1 1 auto;
  }
  .sub {
    font-size: calc(11px * var(--size-ui));
    color: var(--muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
  }
  /* Header search box, right-aligned. */
  .search {
    flex: 0 0 auto;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    background: var(--hover);
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 3px 6px;
    max-width: 46%;
  }
  .search:focus-within {
    border-color: var(--accent, #4aa3df);
  }
  .sicon {
    display: flex;
    align-items: center;
    flex: 0 0 auto;
    color: var(--muted);
  }
  .sinput {
    flex: 1 1 auto;
    min-width: 90px;
    background: none;
    border: none;
    outline: none;
    color: var(--fg);
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
    font-size: calc(12px * var(--size-ui));
  }
  .sinput::placeholder {
    color: var(--muted);
  }
  .sx {
    flex: 0 0 auto;
    display: grid;
    place-items: center;
    background: none;
    border: none;
    color: var(--muted);
    cursor: pointer;
    padding: 1px 3px;
    border-radius: 4px;
  }
  .sx:hover {
    color: var(--fg);
    background: var(--bg);
  }
  /* Header Close button (borderless window, so no OS close). */
  .hx {
    flex: 0 0 auto;
    display: grid;
    place-items: center;
    background: none;
    border: none;
    color: var(--muted);
    cursor: pointer;
    padding: 3px 5px;
    border-radius: 4px;
  }
  .hx:hover {
    color: var(--fg);
    background: var(--hover);
  }
  /* Home icon button in the browse breadcrumb. Sizing lives on the component. */
  .crumb.home {
    padding: 2px 4px;
  }
  /* Search results list (replaces the graph). */
  .results {
    flex: 1 1 auto;
    overflow-y: auto;
    margin: 0 10px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .sresults {
    color: var(--fg);
    font-weight: var(--w-semibold);
    font-size: calc(12px * var(--size-ui));
    flex: 0 0 auto;
  }
  /* Best/recent result-order toggle, sitting next to the match count. */
  .sortsel {
    display: flex;
    align-items: center;
    gap: 4px;
    flex: 0 0 auto;
  }
  /* Project filters live on their own line below the count + sort toggle. */
  .pfilterbar {
    padding-top: 0;
  }
  /* Mutually-exclusive project filters over the results. Takes the middle space
     and scrolls horizontally when there are more projects than fit. */
  .pfilters {
    display: flex;
    align-items: center;
    gap: 4px;
    flex: 1 1 auto;
    min-width: 0;
    overflow-x: auto;
    scrollbar-width: none;
  }
  .pfilters::-webkit-scrollbar {
    display: none;
  }
  .ppill {
    flex: 0 0 auto;
    background: var(--hover);
    border: 1px solid var(--edge);
    border-radius: 999px;
    color: var(--muted, #9aa4b2);
    font-size: calc(11px * var(--size-ui));
    line-height: 1;
    padding: 3px 8px;
    cursor: pointer;
    white-space: nowrap;
  }
  .ppill:hover {
    color: var(--fg);
    border-color: var(--accent, #4aa3df);
  }
  .ppill.on {
    background: var(--accent, #4aa3df);
    border-color: var(--accent, #4aa3df);
    color: #0b0f14;
    font-weight: var(--w-semibold);
  }
  .ppill .pn {
    opacity: 0.7;
    font-family: var(--font-num);
    font-size: calc(1em * var(--size-num) / var(--size-ui));
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  .hit {
    display: block;
    width: 100%;
    text-align: left;
    background: #171d26;
    border: 1px solid #2a333f;
    border-radius: 7px;
    padding: 7px 10px;
    cursor: pointer;
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
  }
  .hit:hover {
    border-color: var(--accent, #4aa3df);
    background: var(--hover);
  }
  .hrow {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .hbadge {
    flex: 0 0 auto;
    align-self: center;
    font-size: calc(9.5px * var(--size-ui));
    font-weight: var(--w-semibold);
    letter-spacing: 0.02em;
    text-transform: uppercase;
    padding: 1px 6px;
    border-radius: 999px;
    color: var(--muted);
    background: color-mix(in srgb, var(--muted) 16%, transparent);
  }
  .hbadge.codex {
    color: var(--accent, #4aa3df);
    background: color-mix(in srgb, var(--accent, #4aa3df) 18%, transparent);
  }
  .htitle {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: var(--w-semibold);
    color: var(--fg);
  }
  .hproj {
    flex: 0 0 auto;
    color: var(--muted);
    font-size: calc(11px * var(--size-ui));
    max-width: 40%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .hdates {
    flex: 0 0 auto;
    color: var(--muted);
    font-size: calc(11px * var(--size-num));
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .hsize {
    flex: 0 0 auto;
    color: var(--muted);
    font-size: calc(11px * var(--size-num));
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  .hsnip :global(b) {
    color: var(--fg);
    font-weight: var(--w-bold);
    background: color-mix(in srgb, var(--accent, #4aa3df) 22%, transparent);
    border-radius: 2px;
  }
  .hsnip {
    margin-top: 3px;
    color: var(--muted);
    font-size: calc(11.5px * var(--size-ui));
    line-height: 1.4;
    overflow: hidden;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
    flex-wrap: wrap;
  }
  .crumb {
    /* Inline-flex so a leading or trailing icon centers against the label instead of
       sitting on the text baseline, and so the gap is a real gap rather than the
       collapsing whitespace a glyph left behind. */
    display: inline-flex;
    align-items: center;
    gap: 3px;
    background: none;
    border: none;
    color: var(--muted);
    cursor: pointer;
    padding: 1px 4px;
    border-radius: 4px;
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
    font-size: calc(12px * var(--size-ui));
  }
  .crumb:hover {
    color: var(--fg);
    background: var(--hover);
  }
  .crumb.back {
    color: var(--accent, #4aa3df);
  }
  .sep {
    color: var(--muted);
    opacity: 0.6;
  }
  .grand {
    margin-left: auto;
    font-family: var(--font-num);
    font-size: calc(1em * var(--size-num) / var(--size-ui));
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
    color: var(--fg);
    font-weight: var(--w-semibold);
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
  }
  .asof {
    font-size: calc(10px * var(--size-ui));
    font-weight: var(--w-regular);
    color: var(--muted);
  }
  .map {
    position: relative;
    flex: 1 1 auto;
    margin: 0 10px 8px;
    border: 1px solid #2a333f;
    border-radius: 9px;
    background: #171d26;
    overflow: hidden;
  }
  /* FasterDB tile: 3px radius, dark outline, and a multi-layer inset vignette
     scaled by --sh (the tile's smaller dimension) for the "depth" look. */
  .tile {
    position: absolute;
    border: none;
    border-radius: 3px;
    padding: 0;
    text-align: center;
    color: #fff;
    overflow: hidden;
    cursor: default;
    font: inherit;
    background: linear-gradient(135deg, var(--ta), var(--tb));
    font-size-adjust: var(--font-ui-adj);
    outline: 1px solid rgba(0, 0, 0, 0.45);
    outline-offset: -1px;
    box-shadow:
      inset 0 0 0 1px rgba(0, 0, 0, 0.4),
      inset 0 0 clamp(0.8px, calc(var(--sh, 60px) * 0.04), 6.4px)
        clamp(0px, calc(var(--sh, 60px) * 0.01), 1.6px) rgba(0, 0, 0, 0.9),
      inset 0 0 clamp(1.2px, calc(var(--sh, 60px) * 0.112), 17.6px) rgba(0, 0, 0, 0.55);
    transition: background 0.12s ease;
  }
  .tile.drill {
    cursor: pointer;
  }
  /* Turn a search hit landed on: a pulsing accent ring so it stands out. */
  .tile.hl {
    outline: 2px solid var(--g1, #fffe00);
    outline-offset: -2px;
    z-index: 3;
    animation: hlpulse 1s ease-in-out 3;
  }
  @keyframes hlpulse {
    0%,
    100% {
      box-shadow:
        inset 0 0 0 1px rgba(0, 0, 0, 0.4),
        0 0 0 0 color-mix(in srgb, var(--g1, #fffe00) 70%, transparent);
    }
    50% {
      box-shadow:
        inset 0 0 0 1px rgba(0, 0, 0, 0.4),
        0 0 10px 3px color-mix(in srgb, var(--g1, #fffe00) 70%, transparent);
    }
  }
  /* No border on hover: brighten just the two gradient stops (the box-shadow
     inner glow is unchanged, so it stays dark). */
  .tile:hover {
    z-index: 2;
    background: linear-gradient(
      135deg,
      color-mix(in srgb, var(--ta) 82%, #fff),
      color-mix(in srgb, var(--tb) 82%, #fff)
    );
  }
  .tm-label {
    position: absolute;
    inset: 0;
    padding: 4px 12px;
    pointer-events: none;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: safe center;
    text-align: center;
    line-height: 1.15;
    overflow: hidden;
  }
  /* Shadow lives on each line (not the label) so its em blur resolves against
     that line's own font-size; otherwise smaller lines look over-blurred. */
  .tm-name,
  .tm-cost,
  .tm-time,
  .tm-size,
  .tm-about {
    text-shadow:
      0.5px 0.5px 0.09em rgba(0, 0, 0, 0.95),
      0.5px 0.5px 0.045em rgba(0, 0, 0, 0.9);
  }
  .tm-name {
    font-weight: var(--w-bold);
    color: #fff;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 100%;
    line-height: 1.4;
  }
  .tm-size {
    color: #fff;
    font-family: var(--font-num);
    font-size: calc(1em * var(--size-num) / var(--size-ui));
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  .tm-time {
    color: rgba(255, 255, 255, 0.72);
    font-family: var(--font-num);
    font-size: calc(1em * var(--size-num) / var(--size-ui));
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .tm-cost {
    color: #fff;
    font-weight: var(--w-bold);
    font-family: var(--font-num);
    font-size: calc(1em * var(--size-num) / var(--size-ui));
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .tm-about {
    margin: 0.4em 0;
    max-width: 92%;
    color: #fff;
    font-weight: var(--w-regular);
    line-height: 1.3;
    white-space: normal;
    overflow: hidden;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
  }
  /* FasterDB-style per-tile hide button: appears on tile hover, top-right. */
  .tm-x {
    position: absolute;
    top: 8px;
    right: 8px;
    width: 18px;
    height: 18px;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border: none;
    border-radius: 4px;
    background: rgba(0, 0, 0, 0.35);
    color: #fff;
    cursor: pointer;
    opacity: 0;
    transition: opacity 0.1s, background 0.1s;
  }
  .tile:hover .tm-x {
    opacity: 1;
  }
  .tm-x:hover {
    background: rgba(248, 81, 73, 0.55);
  }
  /* Badge marking a browsed session that already has a /context breakdown and can
     be drilled straight into the baseline. Bottom-left so it clears the ✕. */
  .tm-ctx {
    position: absolute;
    bottom: 6px;
    left: 7px;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    border-radius: 4px;
    background: var(--accent, #4aa3df);
    color: #fff;
    pointer-events: none;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.5);
  }
  .hidden-bar,
  .typebar {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 5px;
    padding: 2px 10px 6px;
  }
  .typebar {
    padding: 2px 10px 4px;
  }
  /* tokens | $ selector: the lit one sizes the rects. */
  .metricsel {
    display: inline-flex;
    gap: 2px;
  }
  /* Level count ("12 projects") beside the browse metric toggle. */
  .bcount {
    color: var(--muted);
    margin-right: 4px;
  }
  .mbtn {
    background: none;
    border: 1px solid transparent;
    color: var(--muted);
    cursor: pointer;
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
    font-family: var(--font-num);
    font-size: calc(1em * var(--size-num) / var(--size-ui));
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
    padding: 0 6px;
    border-radius: 5px;
  }
  .mbtn:hover {
    color: var(--fg);
  }
  .mbtn.on {
    color: var(--fg);
    border-color: var(--edge);
    background: var(--hover);
    font-weight: var(--w-semibold);
  }
  /* Multi-select block-type filter pills. Accent = the block author's theme color. */
  .tpill {
    background: #171d26;
    border: 1px solid var(--accent);
    color: var(--muted);
    cursor: pointer;
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
    font-size: calc(10px * var(--size-ui));
    padding: 1px 7px;
    border-radius: 4px;
    opacity: 0.6;
  }
  .tpill:hover {
    opacity: 0.85;
  }
  .tpill.on {
    background: color-mix(in srgb, var(--accent) 26%, transparent);
    color: var(--fg);
    font-weight: var(--w-semibold);
    opacity: 1;
  }
  .tclear {
    margin-left: 4px;
  }
  .hlabel {
    font-size: calc(10px * var(--size-ui));
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--muted);
    opacity: 0.7;
  }
  .pill {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    background: #171d26;
    border: 1px solid #2a333f;
    color: var(--fg);
    cursor: pointer;
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
    font-size: calc(11px * var(--size-ui));
    padding: 2px 8px;
    border-radius: 999px;
  }
  .pill:hover {
    border-color: #4aa3df;
  }
  .pill .px {
    color: var(--muted);
    font-size: calc(0.72em * var(--size-ui));
  }
  .pill:hover .px {
    color: #f85149;
  }
  footer {
    display: flex;
    flex-wrap: nowrap;
    align-items: center;
    gap: 10px;
    padding: 4.2px 6px;
    border-top: 1px solid var(--edge);
    font-size: calc(11px * var(--size-ui));
    color: var(--muted);
    min-height: 30px;
    overflow: hidden;
  }
  .legend {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex: none;
    white-space: nowrap;
  }
  .scale {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    flex: none;
  }
  .slabel {
    font-size: calc(10px * var(--size-ui));
    opacity: 0.8;
  }
  .sbar {
    width: 90px;
    height: 8px;
    border-radius: 4px;
  }
  .hover-info {
    margin-left: auto;
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
    flex: 1 1 auto;
    justify-content: flex-end;
    text-align: right;
  }
  .hname {
    font-weight: var(--w-semibold);
    color: var(--fg);
    flex: none;
    white-space: nowrap;
  }
  .hmeta {
    font-family: var(--font-num);
    font-size: calc(1em * var(--size-num) / var(--size-ui));
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
    flex: none;
    white-space: nowrap;
  }
  .hpreview {
    min-width: 0;
    max-width: 46ch;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--muted);
    opacity: 0.85;
    font-style: italic;
  }
  .msg {
    flex: 1 1 auto;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--muted);
  }
  .err {
    color: var(--red);
    padding: 16px;
    text-align: center;
  }
  .empty {
    flex: 1 1 auto;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    padding: 24px;
    text-align: center;
  }
  .etitle {
    font-size: calc(15px * var(--size-ui));
    font-weight: var(--w-semibold);
    color: var(--fg);
  }
  .ebody {
    margin: 0;
    /* Grow with the window instead of capping at a fixed 420px, so the text
       flows further out as the user widens the window. */
    max-width: min(88%, 900px);
    font-size: calc(12.5px * var(--size-ui));
    line-height: 1.5;
    color: var(--muted);
  }
  .empty code {
    font-family: var(--font-mono);
    font-size-adjust: var(--font-mono-adj);
    background: var(--hover);
    color: var(--fg);
    padding: 1px 5px;
    border-radius: 4px;
    font-size: calc(0.92em * var(--size-mono));
  }
  .echk {
    margin-top: 6px;
    background: var(--accent, #4aa3df);
    color: #fff;
    border: none;
    border-radius: 6px;
    padding: 8px 16px;
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
    font-weight: var(--w-semibold);
    cursor: pointer;
  }
  .echk:hover:not(:disabled) {
    filter: brightness(1.1);
  }
  .echk:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .cmsg {
    max-width: min(88%, 900px);
    font-size: calc(12px * var(--size-ui));
    line-height: 1.45;
    color: #f0b57a;
  }
  .cmsg.inline {
    max-width: 340px;
    margin-left: auto;
    text-align: right;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  /* --- Browse (zoom-out) additions --- */
  .crumb.cur {
    color: var(--fg);
    font-weight: var(--w-semibold);
  }
  .crumb.ellip {
    cursor: default;
    opacity: 0.7;
  }
  .crumb.ellip:hover {
    background: none;
    color: var(--muted);
  }
  /* Full-window turn-detail view (replaces the map, not an overlay). */
  .detail {
    flex: 1 1 auto;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .dhead {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--edge);
    /* Author accent as a left rail on the header. */
    border-left: 3px solid var(--accent);
  }
  .dback {
    flex: 0 0 auto;
  }
  .dauthor {
    font-weight: var(--w-bold);
    font-size: calc(15px * var(--size-ui));
    color: var(--accent);
  }
  .dturn {
    color: var(--muted);
    font-size: calc(12px * var(--size-num));
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  .dnav {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .dstep:disabled {
    opacity: 0.35;
    cursor: default;
  }
  .dpos {
    font-size: calc(11px * var(--size-num));
    color: var(--muted);
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
    min-width: 3.5em;
    text-align: center;
  }
  .dx {
    display: grid;
    place-items: center;
    background: none;
    border: none;
    color: var(--muted);
    cursor: pointer;
    padding: 2px 6px;
    border-radius: 4px;
  }
  .dx:hover {
    color: var(--fg);
    background: var(--hover);
  }
  .dmeta {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 18px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--edge);
    background: var(--hover);
  }
  .mrow {
    display: flex;
    flex-direction: column;
    line-height: 1.2;
  }
  .mk {
    font-size: calc(10px * var(--size-ui));
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--muted);
  }
  .mv {
    font-size: calc(12px * var(--size-num));
    color: var(--fg);
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  .dbody {
    margin: 0;
    flex: 1 1 auto;
    min-height: 0;
    padding: 12px 14px;
    overflow: auto;
    white-space: pre-wrap;
    word-break: break-word;
    font-family: var(--font-mono);
    font-size-adjust: var(--font-mono-adj);
    font-size: calc(12px * var(--size-mono));
    line-height: 1.5;
    color: var(--fg);
  }
  .rescan {
    margin-left: 8px;
    font-size: calc(11px * var(--size-ui));
  }
  .px {
    display: flex;
    align-items: center;
    color: var(--muted);
  }
  .rescan:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .deepopt {
    display: flex;
    align-items: center;
    gap: 8px;
    max-width: min(88%, 900px);
    font-size: calc(12px * var(--size-ui));
    color: var(--muted);
    cursor: pointer;
  }
  .scanwrap {
    padding: 4px 10px 0;
  }
</style>
