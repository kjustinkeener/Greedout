import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, fireEvent, waitFor } from "@testing-library/svelte";
import type { SpendEvent, SpendSummary } from "../../types";

// --- Backend + window stubs -------------------------------------------------
// DailySpend now pulls the months/days overview from the tiny `get_spend_summary`
// aggregate, and lazily fetches one day's turns via `get_spend_day` only when a day
// is opened (for the swim lanes). `browse_scan` / `browse_cancel` are fire-and-
// forget. We hand back fixtures keyed by command name and swallow the rest.
const state = vi.hoisted(() => ({ summary: [] as unknown[], day: [] as unknown[] }));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === "get_spend_summary") return state.summary;
    if (cmd === "get_spend_day") return state.day;
    return undefined;
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({
  // Never fires in these tests; just resolve to a no-op unlisten.
  listen: vi.fn(async () => () => {}),
}));
// The title bar tracks maximized state on mount (watchMaximized); stub the
// window API so it resolves quietly instead of touching a real Tauri window.
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    minimize: vi.fn(),
    toggleMaximize: vi.fn(),
    close: vi.fn(),
    isMaximized: vi.fn(async () => false),
    onResized: vi.fn(async () => () => {}),
  }),
}));
// Clicking a lane opens the Explorer window; keep it inert so no Tauri window API
// is touched. (These tests never click a lane, but the import must resolve.)
vi.mock("../explorerWindow", () => ({ openExplorer: vi.fn(async () => {}) }));

import DailySpend from "../DailySpend.svelte";

// A single local day so the drill-down is deterministic: one month bar, one
// populated day bar, then the swim lanes we actually want to exercise.
const DAY = new Date(2026, 8, 8, 12, 0, 0).getTime(); // Sep 8 2026, local noon

// Overview aggregate for that day. Two project leaves ("MoonPool" merged across
// its two paths, and "Greedout"): drives the month/day bars.
const SUMMARY: SpendSummary[] = [
  { day: "2026-09-08", project: "MoonPool", cost: 1.5 },
  { day: "2026-09-08", project: "Greedout", cost: 0.5 },
];

// The historical crash fixture (the day's raw turns): TWO different project *paths*
// that share the same leaf name ("MoonPool"), plus one project worked by BOTH
// harnesses. Returned by get_spend_day when the day is opened.
const DAY_EVENTS: SpendEvent[] = [
  { t: DAY, cost: 1.0, session: "s1", project: "MoonPool", projectPath: "C:/dev/alpha/MoonPool", harness: "claude-code", title: "Alpha", mtime: DAY },
  { t: DAY + 60_000, cost: 0.5, session: "s2", project: "MoonPool", projectPath: "C:/other/beta/MoonPool", harness: "claude-code", title: "Beta", mtime: DAY + 60_000 },
  { t: DAY + 120_000, cost: 0.3, session: "s3", project: "Greedout", projectPath: "C:/dev/greedout", harness: "claude-code", title: "Claude sess", mtime: DAY + 120_000 },
  { t: DAY + 180_000, cost: 0.2, session: "s4", project: "Greedout", projectPath: "C:/dev/greedout", harness: "codex", title: "Codex sess", mtime: DAY + 180_000 },
];

// Drill months -> day: click the one populated month bar, then the one populated
// day bar, landing on the 24h swim-lane view. A duplicate `{#each}` key anywhere
// on the path (months segs, day segs, or lanes) throws `each_key_duplicate`
// during the flush, which surfaces here as a rejected fireEvent / timed-out
// waitFor -> the test fails. That is exactly the regression we are guarding.
async function drillToDay(container: HTMLElement) {
  await waitFor(() => expect(container.querySelector(".chart")).toBeTruthy());
  const monthBar = container.querySelector<HTMLButtonElement>(".bcol:not(.empty)");
  expect(monthBar).toBeTruthy();
  await fireEvent.click(monthBar!);

  await waitFor(() => expect(container.querySelector(".chart.dense")).toBeTruthy());
  const dayBar = container.querySelector<HTMLButtonElement>(".chart.dense .bcol:not(.empty)");
  expect(dayBar).toBeTruthy();
  await fireEvent.click(dayBar!);

  await waitFor(() => expect(container.querySelector(".lane")).toBeTruthy());
}

describe("DailySpend", () => {
  beforeEach(() => {
    state.summary = SUMMARY;
    state.day = DAY_EVENTS;
  });

  it("renders the months view without throwing and merges same-leaf projects into one segment", async () => {
    const { container } = render(DailySpend);
    await waitFor(() => expect(container.querySelector(".chart")).toBeTruthy());
    const monthBars = container.querySelectorAll(".bcol");
    expect(monthBars.length).toBe(1); // all fixture spend is in one month
    // Month bar segments are keyed by leaf project name; the summary already merges
    // the two "MoonPool" paths into one row, so there are exactly 2 distinct
    // leaves => 2 segs, and the segs `{#each (s.project)}` key stays unique.
    const segs = monthBars[0].querySelectorAll(".seg");
    expect(segs.length).toBe(2); // MoonPool (merged) + Greedout
  });

  it("guards the each_key_duplicate crash: two same-leaf different-path projects render as two distinct lanes", async () => {
    const { container } = render(DailySpend);
    await drillToDay(container);

    const lanes = container.querySelectorAll(".lane");
    // Three project PATHS on this day => three lanes. If lanes were keyed by the
    // leaf name (`l.project`) instead of the full path (`l.key`), the two
    // "MoonPool" lanes would collide, Svelte would throw each_key_duplicate, and
    // this drill-down would never reach three lanes.
    expect(lanes.length).toBe(3);

    // Two of those lanes are labelled "MoonPool" -- proof the same-leaf pair both
    // survived as separate rows rather than merging or crashing.
    const labels = [...container.querySelectorAll(".lname")].map((n) => n.textContent?.trim());
    expect(labels.filter((l) => l === "MoonPool").length).toBe(2);
    expect(labels).toContain("Greedout");
  });

  it("keeps a single lane for one path worked by two harnesses (chooser offers both sessions)", async () => {
    const { container } = render(DailySpend);
    await drillToDay(container);

    // The Greedout path ran under both Claude and Codex; it must stay ONE lane
    // whose meta says it covers 2 sessions (the chooser disambiguates them).
    const metas = [...container.querySelectorAll<HTMLButtonElement>(".lmeta")];
    const greedout = metas.find((m) => m.querySelector(".lname")?.textContent?.trim() === "Greedout");
    expect(greedout).toBeTruthy();
    expect(greedout!.title).toContain("2 sessions");
  });

  it("shows the empty-state message when there is no spend", async () => {
    state.summary = [];
    const { container } = render(DailySpend);
    await waitFor(() => expect(container.querySelector(".msg")).toBeTruthy());
    expect(container.querySelector(".chart")).toBeNull();
  });
});
