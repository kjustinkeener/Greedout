<script lang="ts">
  // Daily Spend: a three-level drill-down over every billed turn across all
  // sessions. Months -> days-in-month -> a 24-hour swim-lane of that day, one lane
  // per session, spans merged across activity gaps shorter than an hour.
  //
  // The months + days overviews run off a tiny per-day/per-project AGGREGATE
  // (get_spend_summary): the backend has already deduped resume/compaction copies
  // and summed by local day, so every bar total here is safe to take directly. The
  // swim lanes need the individual turns, so one day's events are fetched lazily
  // (get_spend_day) only when a day is opened, and cached per day.
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import type { SpendEvent, SpendSummary } from "../types";
  import Brand from "./Brand.svelte";
  import Icon from "./Icon.svelte";
  import ScanProgress from "./ScanProgress.svelte";
  import { scanState, startScan, cancelScan, onScanDone } from "./scanControl";
  import { openExplorer } from "./explorerWindow";
  import { t, watchLocale, activeLocale } from "./i18n.svelte";

  let summary = $state<SpendSummary[]>([]);
  // The open day's individual turns (for the swim lanes), fetched on demand.
  let dayEvents = $state<SpendEvent[]>([]);
  // Per-day cache of fetched turns, keyed by "YYYY-MM-DD".
  const dayCache = new Map<string, SpendEvent[]>();
  let loading = $state(true);
  // Scan state (the shared cache build, reused from the Context Explorer scan)
  // lives in the shared scanControl store; `running` gates the empty-state
  // message and the rescan button here.
  const scanning = $derived($scanState.running);
  let level = $state<"months" | "days" | "day">("months");
  let curMonth = $state(""); // "YYYY-MM"
  let curDay = $state(""); // "YYYY-MM-DD"
  let hover = $state<string>(""); // free-form readout for the hovered bar/span
  // When a clicked lane holds more than one session, this holds it so a chooser
  // can list them; null when no chooser is open.
  let chooser = $state<Lane | null>(null);

  const HOUR = 3_600_000;
  const DAY = 86_400_000;

  // This window can be open while the language changes in Settings.
  const unlistenLocale = watchLocale();
  onDestroy(() => void unlistenLocale.then((u) => u()));

  async function fetchSummary() {
    try {
      summary = await invoke<SpendSummary[]>("get_spend_summary");
    } catch {
      summary = [];
    }
  }
  // Fetch (or reuse a cached) one day's turns for the swim lanes. A later
  // openDay/stepDay wins: a response whose key is no longer the open day is ignored.
  async function loadDay(key: string) {
    if (!key) return;
    const cached = dayCache.get(key);
    if (cached) {
      if (curDay === key) dayEvents = cached;
      return;
    }
    const lo = dayStartMs(key);
    const hi = lo + DAY;
    try {
      const evs = await invoke<SpendEvent[]>("get_spend_day", { lo, hi });
      dayCache.set(key, evs);
      if (curDay === key) dayEvents = evs;
    } catch {
      if (curDay === key) dayEvents = [];
    }
  }
  onMount(async () => {
    // Read the small summary aggregate FIRST, off the warm cache, before the scan
    // opens its write transaction: the scan's write lock would make a concurrent
    // read fail to empty (a blank "0 rows" flash). The summary is fast even on a
    // large history, so the window shows real data in well under a second, then the
    // scan folds in anything new (the onScanDone handler below refetches).
    await fetchSummary();
    loading = false;
    startScan();
  });

  // Scan bars/log render off the shared scanControl store (via <ScanProgress>);
  // this only handles the DATA side-effects when a scan finishes.
  const unScanDone = onScanDone(() => {
    void fetchSummary(); // pull the freshly-written aggregate
    // If a day is open, its cached turns are now stale -- drop and reload it.
    if (level === "day" && curDay) {
      dayCache.delete(curDay);
      void loadDay(curDay);
    }
  });
  onDestroy(unScanDone);

  // --- Local-time keys. Bucketing is by the user's clock, not UTC: "daily spend"
  // means the day the user was working, and a turn at 23:30 belongs to that day. ---
  const pad = (n: number) => String(n).padStart(2, "0");
  function dayStartMs(key: string): number {
    const [y, m, d] = key.split("-").map(Number);
    return new Date(y, m - 1, d).getTime();
  }

  // --- Formatters (locale-aware; the only "copy" the drill-downs need). ---
  function usd(v: number): string {
    const loc = activeLocale();
    const digits = v !== 0 && Math.abs(v) < 100 ? 2 : 0;
    return new Intl.NumberFormat(loc, {
      style: "currency",
      currency: "USD",
      minimumFractionDigits: digits,
      maximumFractionDigits: digits,
    }).format(v);
  }
  function monthLabel(key: string, withYear: boolean): string {
    const [y, m] = key.split("-").map(Number);
    const d = new Date(y, m - 1, 1);
    return new Intl.DateTimeFormat(activeLocale(), {
      month: "short",
      ...(withYear ? { year: "numeric" } : {}),
    }).format(d);
  }
  function monthLong(key: string): string {
    const [y, m] = key.split("-").map(Number);
    return new Intl.DateTimeFormat(activeLocale(), { month: "long", year: "numeric" }).format(
      new Date(y, m - 1, 1),
    );
  }
  function dayLong(key: string): string {
    const [y, m, d] = key.split("-").map(Number);
    return new Intl.DateTimeFormat(activeLocale(), {
      weekday: "short",
      month: "short",
      day: "numeric",
    }).format(new Date(y, m - 1, d));
  }
  function clock(ms: number): string {
    return new Intl.DateTimeFormat(activeLocale(), { hour: "2-digit", minute: "2-digit" }).format(
      new Date(ms),
    );
  }
  // Short date for a session's last turn in the chooser (e.g. "Sep 8").
  function dateAbbr(ms: number): string {
    return new Intl.DateTimeFormat(activeLocale(), { month: "short", day: "numeric" }).format(
      new Date(ms),
    );
  }

  const grandTotal = $derived(summary.reduce((a, r) => a + r.cost, 0));

  // Deterministic per-project color, stable across all three levels: same project
  // is the same hue in every stacked bar and every swim-lane span.
  function projColor(p: string): string {
    let h = 0;
    for (let i = 0; i < p.length; i++) h = (h * 31 + p.charCodeAt(i)) >>> 0;
    return `hsl(${h % 360} 65% 55%)`;
  }
  // Place a swim lane along the theme gauge gradient (g0 -> g1 -> g2). Lanes are
  // sorted highest-total first, so index 0 maps to the hot end (g2): heavier spend
  // reads hotter, matching the gauge. color-mix keeps it live with theme changes
  // and needs no color parsing. (Flip the p formula to 1 - that to reverse.)
  function laneColor(i: number, n: number): string {
    const p = n <= 1 ? 1 : (n - 1 - i) / (n - 1);
    return p <= 0.5
      ? `color-mix(in oklab, var(--g1) ${p * 200}%, var(--g0))`
      : `color-mix(in oklab, var(--g2) ${(p - 0.5) * 200}%, var(--g1))`;
  }
  interface Seg {
    project: string;
    cost: number;
  }
  // Per-project segments of one bar, largest at the bottom of the stack.
  function segsOf(byproj: Map<string, number>): Seg[] {
    return [...byproj]
      .map(([project, cost]) => ({ project, cost }))
      .sort((a, b) => b.cost - a.cost);
  }

  // --- Level 1: months (continuous timeline, gaps filled with zero bars). ---
  interface MonthBar {
    key: string;
    total: number;
    count: number;
    segs: Seg[];
  }
  const months = $derived.by<MonthBar[]>(() => {
    const m = new Map<string, { total: number; count: number; byproj: Map<string, number> }>();
    for (const r of summary) {
      const k = r.day.slice(0, 7);
      const o = m.get(k) ?? { total: 0, count: 0, byproj: new Map() };
      o.total += r.cost;
      o.count++;
      o.byproj.set(r.project, (o.byproj.get(r.project) ?? 0) + r.cost);
      m.set(k, o);
    }
    const keys = [...m.keys()].sort();
    if (!keys.length) return [];
    // Fill every month between the first and last so the axis reads as a timeline
    // rather than skipping quiet months.
    const out: MonthBar[] = [];
    let [y, mo] = keys[0].split("-").map(Number);
    const [ly, lm] = keys[keys.length - 1].split("-").map(Number);
    while (y < ly || (y === ly && mo <= lm)) {
      const k = `${y}-${pad(mo)}`;
      const o = m.get(k);
      out.push({ key: k, total: o?.total ?? 0, count: o?.count ?? 0, segs: o ? segsOf(o.byproj) : [] });
      mo++;
      if (mo > 12) {
        mo = 1;
        y++;
      }
    }
    return out;
  });
  const monthsMax = $derived(Math.max(1e-9, ...months.map((b) => b.total)));

  // --- Level 2: days of the selected month (1..N, gaps filled). ---
  interface DayBar {
    key: string;
    day: number;
    total: number;
    count: number;
    segs: Seg[];
  }
  const daysInMonth = $derived.by(() => {
    if (!curMonth) return 0;
    const [y, m] = curMonth.split("-").map(Number);
    return new Date(y, m, 0).getDate();
  });
  const days = $derived.by<DayBar[]>(() => {
    if (!curMonth) return [];
    const totals = new Array(daysInMonth + 1).fill(0);
    const counts = new Array(daysInMonth + 1).fill(0);
    const byproj: Map<string, number>[] = Array.from({ length: daysInMonth + 1 }, () => new Map());
    for (const r of summary) {
      if (!r.day.startsWith(curMonth)) continue;
      const d = Number(r.day.slice(8, 10));
      totals[d] += r.cost;
      counts[d]++;
      byproj[d].set(r.project, (byproj[d].get(r.project) ?? 0) + r.cost);
    }
    const out: DayBar[] = [];
    for (let d = 1; d <= daysInMonth; d++) {
      out.push({
        key: `${curMonth}-${pad(d)}`,
        day: d,
        total: totals[d],
        count: counts[d],
        segs: segsOf(byproj[d]),
      });
    }
    return out;
  });
  const daysMax = $derived(Math.max(1e-9, ...days.map((b) => b.total)));
  const monthTotal = $derived(
    curMonth ? summary.reduce((a, r) => a + (r.day.startsWith(curMonth) ? r.cost : 0), 0) : 0,
  );

  // --- Level 3: 24-hour swim lanes for the selected day. One lane per session;
  // consecutive turns join into one span unless separated by a gap of >= 1 hour. ---
  interface Span {
    start: number;
    end: number;
    cost: number;
    count: number;
  }
  // One session that contributed to a lane, for the chooser when a lane (project)
  // holds more than one session's turns.
  interface SessionSlice {
    id: string;
    title: string;
    harness: string; // "claude-code" or "codex", for the chooser badge
    cost: number;
    last: number; // session's overall last-activity time (mtime), epoch ms
  }
  // Short harness label for the chooser badge (mirrors Baseline.svelte).
  const harnessLabel = (h: string): string => (h === "codex" ? "Codex" : "Claude");
  interface Lane {
    key: string; // full project path (unique); `project` is only the leaf label
    project: string;
    spans: Span[];
    total: number;
    first: number;
    sessions: SessionSlice[]; // distinct sessions in this lane, cost desc
  }
  // One lane per PROJECT for the day: all of that project's turns (across every
  // session it ran that day) merge into a single row, then split into spans
  // wherever there's a gap of an hour or more.
  const lanes = $derived.by<Lane[]>(() => {
    if (!curDay) return [];
    // Group by FULL project path, not the leaf name: two unrelated folders that
    // share a basename must not collapse, and a folder worked by both Claude and
    // Codex stays ONE lane (the chooser's harness badges tell its sessions apart).
    // Fall back to the leaf name for rows with no path (e.g. Codex without a cwd).
    const byProject = new Map<string, SpendEvent[]>();
    for (const e of dayEvents) {
      const key = e.projectPath || e.project;
      const arr = byProject.get(key) ?? [];
      arr.push(e);
      byProject.set(key, arr);
    }
    const out: Lane[] = [];
    for (const [laneKey, evs] of byProject) {
      const project = evs[0].project; // leaf name, shown as the lane label
      evs.sort((a, b) => a.t - b.t);
      const spans: Span[] = [];
      let cur: Span | null = null;
      for (const e of evs) {
        if (cur && e.t - cur.end < HOUR) {
          cur.end = e.t;
          cur.cost += e.cost;
          cur.count++;
        } else {
          if (cur) spans.push(cur);
          cur = { start: e.t, end: e.t, cost: e.cost, count: 1 };
        }
      }
      if (cur) spans.push(cur);
      // Break the lane down by session so a lane covering more than one session
      // can offer a chooser rather than guessing which to open.
      const bySession = new Map<string, SessionSlice>();
      for (const e of evs) {
        const o = bySession.get(e.session) ?? { id: e.session, title: e.title, harness: e.harness, cost: 0, last: e.mtime };
        if (!o.title && e.title) o.title = e.title;
        o.cost += e.cost;
        if (e.mtime > o.last) o.last = e.mtime;
        bySession.set(e.session, o);
      }
      out.push({
        key: laneKey,
        project,
        spans,
        total: spans.reduce((a, b) => a + b.cost, 0),
        first: evs[0].t,
        sessions: [...bySession.values()].sort((a, b) => b.cost - a.cost),
      });
    }
    out.sort((a, b) => b.total - a.total);
    return out;
  });
  const dayTotal = $derived(lanes.reduce((a, l) => a + l.total, 0));
  // Header total tracks the drill level: all sessions → the open month → the open day.
  const headerTotal = $derived(level === "day" ? dayTotal : level === "days" ? monthTotal : grandTotal);
  const headerTitle = $derived(
    level === "day" ? dayLong(curDay) : level === "days" ? monthLong(curMonth) : "Total across all sessions",
  );
  const dayStart = $derived(curDay ? dayStartMs(curDay) : 0);
  const HOURS = [0, 3, 6, 9, 12, 15, 18, 21, 24];

  // Fraction across the 24h day [0,1] for a timestamp, clamped.
  const frac = (ms: number) => Math.min(1, Math.max(0, (ms - dayStart) / DAY));

  // --- Navigation ---
  function openMonth(key: string) {
    curMonth = key;
    level = "days";
    hover = "";
  }
  function openDay(key: string) {
    curDay = key;
    level = "day";
    hover = "";
    void loadDay(key);
  }
  // Open the Context Explorer for one session of a lane. Title mirrors the main
  // window's clickable-title format ("<session> · <project>").
  function openLaneSession(project: string, s: SessionSlice) {
    void openExplorer({
      id: s.id,
      title: `${s.title || project} · ${project}`,
      project,
      view: "session",
    });
  }
  // Click a lane: open its Explorer directly if it's a single session, else pop a
  // chooser listing the sessions that shared the lane.
  function pickLane(l: Lane) {
    if (l.sessions.length <= 1) {
      if (l.sessions[0]) openLaneSession(l.project, l.sessions[0]);
      return;
    }
    chooser = l;
  }
  // Keys that actually have spend, sorted (string order is chronological for
  // "YYYY-MM" / "YYYY-MM-DD"). Stepping walks these, so nav never lands on an
  // empty month/day and stops at the ends of the data range.
  const monthList = $derived([...new Set(summary.map((r) => r.day.slice(0, 7)))].sort());
  const dayList = $derived([...new Set(summary.map((r) => r.day))].sort());
  function stepIn(list: string[], cur: string, delta: number): string {
    if (!list.length) return cur;
    const idx = list.indexOf(cur);
    if (idx === -1) {
      // Current key has no data: jump to the nearest populated one in this direction.
      return delta > 0
        ? (list.find((k) => k > cur) ?? cur)
        : ([...list].reverse().find((k) => k < cur) ?? cur);
    }
    const j = idx + delta;
    return j >= 0 && j < list.length ? list[j] : cur;
  }
  function stepMonth(delta: number) {
    const next = stepIn(monthList, curMonth, delta);
    if (next === curMonth) return;
    curMonth = next;
    hover = "";
  }
  function stepDay(delta: number) {
    const next = stepIn(dayList, curDay, delta);
    if (next === curDay) return;
    curDay = next;
    curMonth = next.slice(0, 7); // keep the breadcrumb month in sync
    hover = "";
    void loadDay(next);
  }
  function toMonths() {
    level = "months";
    hover = "";
  }
  // Right-click anywhere zooms out one level, like the Context Explorer treemap.
  function zoomOut(e: MouseEvent) {
    e.preventDefault();
    if (level === "day") toDays();
    else if (level === "days") toMonths();
  }
  function toDays() {
    if (curMonth) {
      level = "days";
      hover = "";
    }
  }
  // Left/Right arrows drive the same prev/next stepping as the nav buttons:
  // day view steps by day, days view steps by month, months view has no nav.
  function onKey(e: KeyboardEvent) {
    // While the chooser is open it owns the keyboard: Escape closes it, and the
    // arrow keys must not step the day/month underneath it.
    if (chooser) {
      if (e.key === "Escape") {
        chooser = null;
        e.preventDefault();
      } else if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
        e.preventDefault();
      }
      return;
    }
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    const delta = e.key === "ArrowLeft" ? -1 : 1;
    if (level === "day") stepDay(delta);
    else if (level === "days") stepMonth(delta);
    else return;
    e.preventDefault();
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="wrap" oncontextmenu={zoomOut} role="presentation">
  <header class="bar">
    <Brand size={15} font={14} />
    <span class="total" title={headerTitle}>{usd(headerTotal)}</span>
    <button
      class="rescan"
      onclick={() => startScan()}
      disabled={scanning}
      title={scanning ? "Scanning…" : "Rescan"}
      aria-label="Rescan"
    >
      <Icon name="refresh" size={13} />
    </button>
  </header>
  <nav class="crumbs" aria-label="breadcrumb">
    <button class="crumb" class:active={level === "months"} onclick={toMonths}>
      <Icon name="home" size={12} /> All months
    </button>
    {#if level !== "months"}
      <span class="sep"><Icon name="chevron-right" size={11} /></span>
      <button class="crumb" class:active={level === "days"} onclick={toDays}>
        {monthLong(curMonth)}
      </button>
    {/if}
    {#if level === "day"}
      <span class="sep"><Icon name="chevron-right" size={11} /></span>
      <span class="crumb active">{dayLong(curDay)}</span>
    {/if}
  </nav>

  <ScanProgress oncancel={cancelScan} />

  {#if loading}
    <div class="msg">{t("common.loading")}</div>
  {:else if !summary.length}
    <!-- The scan bars (above) show progress during the first build; only show the
         resting empty message once the scan is done with no spend. -->
    {#if !scanning}
      <div class="msg">No spend recorded yet.</div>
    {/if}
  {:else if level === "months"}
    <div class="sub">
      <span class="readout">{hover || "Click a month to break it down by day"}</span>
    </div>
    <div class="chart">
      {#each months as b, i (b.key)}
        <button
          class="bcol"
          class:empty={b.total === 0}
          onclick={() => b.total > 0 && openMonth(b.key)}
          onmouseenter={() => (hover = `${monthLong(b.key)} · ${usd(b.total)}`)}
          onmouseleave={() => (hover = "")}
          title={`${monthLong(b.key)} · ${usd(b.total)}`}
        >
          <span class="bwrap">
            {#each b.segs as s (s.project)}
              <span
                class="seg"
                style:height={`${(s.cost / monthsMax) * 100}%`}
                style:background={projColor(s.project)}
                title={`${s.project} · ${usd(s.cost)}`}
              ></span>
            {/each}
          </span>
          <span class="blabel"
            >{monthLabel(b.key, i === 0 || b.key.endsWith("-01"))}</span
          >
        </button>
      {/each}
    </div>
  {:else if level === "days"}
    <div class="sub">
      <button class="nav" aria-label="Previous month" title="Previous month" onclick={() => stepMonth(-1)}>
        <Icon name="chevron-left" size={15} />
      </button>
      <span class="readout">{hover || `${monthLong(curMonth)} · ${usd(monthTotal)}`}</span>
      <button class="nav" aria-label="Next month" title="Next month" onclick={() => stepMonth(1)}>
        <Icon name="chevron-right" size={15} />
      </button>
    </div>
    <div class="chart dense">
      {#each days as b (b.key)}
        <button
          class="bcol"
          class:empty={b.total === 0}
          onclick={() => b.total > 0 && openDay(b.key)}
          onmouseenter={() => (hover = `${dayLong(b.key)} · ${usd(b.total)}`)}
          onmouseleave={() => (hover = "")}
          title={`${dayLong(b.key)} · ${usd(b.total)}`}
        >
          <span class="bwrap">
            {#each b.segs as s (s.project)}
              <span
                class="seg"
                style:height={`${(s.cost / daysMax) * 100}%`}
                style:background={projColor(s.project)}
                title={`${s.project} · ${usd(s.cost)}`}
              ></span>
            {/each}
          </span>
          <span class="blabel num">{b.day}</span>
        </button>
      {/each}
    </div>
  {:else}
    <div class="sub">
      <button class="nav" aria-label="Previous day" title="Previous day" onclick={() => stepDay(-1)}>
        <Icon name="chevron-left" size={15} />
      </button>
      <span class="readout">{hover || `${dayLong(curDay)} · ${usd(dayTotal)}`}</span>
      <button class="nav" aria-label="Next day" title="Next day" onclick={() => stepDay(1)}>
        <Icon name="chevron-right" size={15} />
      </button>
    </div>
    {#if !lanes.length}
      <div class="msg">No activity this day.</div>
    {:else}
      <div class="lanes">
        <div class="axis">
          {#each HOURS as h}
            <span class="tick" style:left={`${(h / 24) * 100}%`}>{pad(h)}</span>
          {/each}
        </div>
        <div class="lanebody">
          <div class="gridlayer">
            {#each HOURS as h}
              <span class="grid" style:left={`${(h / 24) * 100}%`}></span>
            {/each}
          </div>
          {#each lanes as l, i (l.key)}
            <div class="lane">
              <div class="track">
                {#each l.spans as sp}
                  <span
                    class="span"
                    style:left={`${frac(sp.start) * 100}%`}
                    style:width={`max(3px, ${(frac(sp.end) - frac(sp.start)) * 100}%)`}
                    style:background={laneColor(i, lanes.length)}
                    onclick={() => pickLane(l)}
                    onmouseenter={() =>
                      (hover = `${l.project} · ${clock(sp.start)}–${clock(sp.end)} · ${usd(sp.cost)}`)}
                    onmouseleave={() => (hover = "")}
                    title={`${l.project} · ${clock(sp.start)}–${clock(sp.end)} · ${usd(sp.cost)}`}
                    role="button"
                    tabindex="-1"
                  ></span>
                {/each}
              </div>
              <button
                class="lmeta"
                onclick={() => pickLane(l)}
                title={l.sessions.length > 1
                  ? `${l.project} · ${l.sessions.length} sessions · open in Context Explorer`
                  : `${l.project} · open in Context Explorer`}
              >
                <span class="lname">{l.project}</span>
                <span class="lcost">{usd(l.total)}</span>
              </button>
            </div>
          {/each}
        </div>
      </div>
    {/if}
  {/if}

  {#if chooser}
    <div
      class="chback"
      onclick={() => (chooser = null)}
      oncontextmenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
        chooser = null;
      }}
      role="presentation"
    >
      <div class="chpanel" role="dialog" aria-label="Choose a session" onclick={(e) => e.stopPropagation()}>
        <div class="chhead">
          <span class="chtitle" title={chooser.project}>{chooser.project}</span>
          <button class="chclose" onclick={() => (chooser = null)} aria-label={t("common.close")}>
            <Icon name="x" size={12} />
          </button>
        </div>
        <ul class="chlist">
          {#each chooser.sessions as s (s.id)}
            <li>
              <button
                class="chrow"
                onclick={() => {
                  if (chooser) openLaneSession(chooser.project, s);
                  chooser = null;
                }}
              >
                <span class="chmain">
                  <span class="chname">
                    <span class="hbadge" class:codex={s.harness === "codex"}>{harnessLabel(s.harness)}</span>
                    {s.title || s.id.slice(0, 8)}
                  </span>
                  <span class="chsub">{chooser.project} · {dateAbbr(s.last)}</span>
                </span>
                <span class="chcost">{usd(s.cost)}</span>
              </button>
            </li>
          {/each}
        </ul>
      </div>
    </div>
  {/if}
</div>

<style>
  .wrap {
    position: relative;
    display: flex;
    flex-direction: column;
    height: 100vh;
    color: var(--fg);
    background: var(--bg);
    font-size: 13px;
    overflow: hidden;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--panel);
    flex: none;
  }
  .total {
    margin-left: auto;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
    flex: none;
    overflow-x: auto;
    padding: 8px 12px;
    border-bottom: 1px solid var(--panel);
  }
  .crumb {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    background: none;
    border: none;
    color: var(--muted);
    font: inherit;
    padding: 3px 6px;
    border-radius: 5px;
    cursor: pointer;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  button.crumb:hover {
    background: var(--panel);
    color: var(--fg);
  }
  .crumb.active {
    color: var(--fg);
    cursor: default;
  }
  .sep {
    color: var(--muted);
    opacity: 0.6;
    display: inline-flex;
  }
  .total {
    font-variant-numeric: tabular-nums;
    font-weight: 600;
    color: var(--fg);
    flex: none;
  }
  .rescan {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: 1px solid var(--panel);
    color: var(--muted);
    border-radius: 5px;
    padding: 3px 6px;
    cursor: pointer;
  }
  .rescan:hover:not(:disabled) {
    color: var(--fg);
    border-color: var(--muted);
  }
  .rescan:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .msg {
    margin: auto;
    color: var(--muted);
    padding: 40px;
  }
  .sub {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 14px 2px;
    flex: none;
  }
  .readout {
    flex: 1;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .nav {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    background: none;
    border: none;
    border-radius: 5px;
    color: var(--muted);
    cursor: pointer;
    flex: none;
  }
  .nav:hover {
    background: var(--panel);
    color: var(--fg);
  }

  /* Bar charts (months, days) */
  .chart {
    flex: 1;
    display: flex;
    align-items: flex-end;
    gap: 6px;
    padding: 12px 14px 8px;
    min-height: 0;
    overflow-x: auto;
  }
  .chart.dense {
    gap: 2px;
  }
  .bcol {
    flex: 1 1 0;
    min-width: 14px;
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 5px;
    background: none;
    border: none;
    padding: 0;
    color: var(--muted);
    cursor: pointer;
  }
  .chart.dense .bcol {
    min-width: 8px;
  }
  .bcol.empty {
    cursor: default;
  }
  .bwrap {
    flex: 1;
    width: 100%;
    display: flex;
    flex-direction: column;
    justify-content: flex-end;
    align-items: stretch;
    gap: 1px;
    min-height: 0;
  }
  .seg {
    width: 100%;
    min-height: 1px;
    transition: filter 0.1s;
  }
  /* segs are sorted largest-first, so the first is the topmost -- round its top. */
  .seg:first-child {
    border-radius: 3px 3px 0 0;
  }
  .bcol:hover:not(.empty) .seg {
    filter: brightness(1.15);
  }
  .blabel {
    font-size: 10px;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .blabel.num {
    font-size: 9px;
  }
  .bcol:hover .blabel {
    color: var(--fg);
  }

  /* Swim lanes. A single --labels offset keeps the hour axis, the grid lines and
     the span positions on one shared 24h reference width (the track column), so
     a span at noon lands exactly under the "12" tick. */
  .lanes {
    --labels: 148px;
    flex: 1;
    display: flex;
    flex-direction: column;
    min-height: 0;
    padding: 4px 14px 12px;
    overflow-y: auto;
  }
  .axis {
    position: relative;
    height: 16px;
    margin-right: var(--labels);
    flex: none;
  }
  .tick {
    position: absolute;
    transform: translateX(-50%);
    font-size: 9px;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
    top: 2px;
    white-space: nowrap;
  }
  .tick:first-child {
    transform: none;
  }
  .tick:last-child {
    transform: translateX(-100%);
  }
  .lanebody {
    position: relative;
    flex: 1;
    min-height: 0;
  }
  .gridlayer {
    position: absolute;
    top: 0;
    bottom: 0;
    left: 0;
    right: var(--labels);
    pointer-events: none;
  }
  .grid {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--panel);
    transform: translateX(-0.5px);
  }
  .lane {
    display: grid;
    grid-template-columns: 1fr var(--labels);
    align-items: center;
    gap: 0;
    height: 26px;
  }
  .track {
    position: relative;
    height: 14px;
    background: var(--panel);
    border-radius: 4px;
  }
  .span {
    position: absolute;
    top: 0;
    height: 100%;
    border-radius: 4px;
    background: linear-gradient(to right, var(--g0), var(--g1));
    cursor: pointer;
  }
  .span:hover {
    filter: brightness(1.15);
  }
  .lmeta {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 2px 4px 2px 10px;
    min-width: 0;
    background: none;
    border: none;
    border-radius: 5px;
    font: inherit;
    color: inherit;
    text-align: left;
    cursor: pointer;
  }
  .lmeta:hover {
    background: var(--panel);
  }
  .lmeta:hover .lname {
    color: var(--g0);
  }
  .lname {
    flex: 1;
    font-size: 11px;
    color: var(--fg);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .lcost {
    font-size: 11px;
    color: var(--muted);
    text-align: right;
    font-variant-numeric: tabular-nums;
    flex: none;
  }

  /* Session chooser: shown when a clicked lane covers more than one session. */
  .chback {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: color-mix(in srgb, var(--bg) 55%, transparent);
    z-index: 10;
  }
  .chpanel {
    width: min(340px, 82%);
    max-height: 70%;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    border: 1px solid var(--panel);
    border-radius: 8px;
    box-shadow: 0 8px 28px rgba(0, 0, 0, 0.35);
    overflow: hidden;
  }
  .chhead {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    border-bottom: 1px solid var(--panel);
  }
  .chtitle {
    flex: 1;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .chclose {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    background: none;
    border: none;
    border-radius: 5px;
    color: var(--muted);
    cursor: pointer;
  }
  .chclose:hover {
    background: var(--panel);
    color: var(--fg);
  }
  .chlist {
    margin: 0;
    padding: 4px;
    list-style: none;
    overflow-y: auto;
  }
  .chrow {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 7px 8px;
    background: none;
    border: none;
    border-radius: 5px;
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .chrow:hover {
    background: var(--panel);
  }
  .chmain {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .chname {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .hbadge {
    display: inline-block;
    font-size: 9.5px;
    font-weight: var(--w-semibold);
    letter-spacing: 0.02em;
    text-transform: uppercase;
    padding: 1px 6px;
    border-radius: 999px;
    margin-right: 5px;
    vertical-align: baseline;
    color: var(--muted);
    background: color-mix(in srgb, var(--muted) 16%, transparent);
  }
  .hbadge.codex {
    color: var(--accent, #4aa3df);
    background: color-mix(in srgb, var(--accent, #4aa3df) 18%, transparent);
  }
  .chsub {
    font-size: 11px;
    color: var(--muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .chcost {
    flex: none;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
</style>
