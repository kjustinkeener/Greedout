import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, fireEvent, waitFor } from "@testing-library/svelte";
import { tick } from "svelte";
import type { SearchHit } from "../../types";

// --- Backend + window stubs -------------------------------------------------
// Baseline is window-shaped: it primes a browse index, analyzes a session, and
// wires several Tauri event listeners. We give it just enough to mount at the
// browse-root level, and a controllable event bus so we can push search hits.
const bus = vi.hoisted(() => {
  const listeners = new Map<string, ((e: { payload: unknown }) => void)[]>();
  return {
    listeners,
    emit(name: string, payload: unknown) {
      for (const cb of listeners.get(name) ?? []) cb({ payload });
    },
    reset() {
      listeners.clear();
    },
  };
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    switch (cmd) {
      case "browse_status":
        return { enabled: true, dbExists: true, indexed: 3, enriched: 3, lastFullScan: null, scanning: false };
      case "browse_harnesses":
        return [];
      case "browse_projects":
      case "browse_sessions":
        return [];
      case "session_harness":
        return "claude-code";
      case "analyze_baseline":
        return { ok: true, message: "", needsContext: false, total: 1000, max: 200000, capturedAt: "", costRate: 0, costIn: 0, costOut: 0, nodes: [] };
      case "chat_breakdown":
        return [];
      default:
        return undefined;
    }
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
    const arr = bus.listeners.get(name) ?? [];
    arr.push(cb);
    bus.listeners.set(name, arr);
    return () => {
      const cur = bus.listeners.get(name) ?? [];
      bus.listeners.set(name, cur.filter((c) => c !== cb));
    };
  }),
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    close: vi.fn(),
    minimize: vi.fn(),
    toggleMaximize: vi.fn(),
    isMaximized: vi.fn(async () => false),
    onResized: vi.fn(async () => () => {}),
  }),
}));

import Baseline from "../Baseline.svelte";

function mkHit(over: Partial<SearchHit>): SearchHit {
  return {
    id: "sess",
    title: "A session",
    project: "Proj",
    projectPath: "C:/x/Proj",
    sizeBytes: 1234,
    mtime: 0,
    firstMs: 0,
    lastMs: 0,
    score: 1,
    snippet: "a snippet",
    harness: "claude-code",
    ...over,
  };
}

// Type "foo" and hit Enter to switch Baseline into the search-results view, then
// stream hits in through the mocked "search-hit" event bus.
async function openSearch(container: HTMLElement) {
  await waitFor(() => expect(container.querySelector(".sinput")).toBeTruthy());
  const input = container.querySelector<HTMLInputElement>(".sinput")!;
  await fireEvent.input(input, { target: { value: "foo" } });
  await fireEvent.keyDown(input, { key: "Enter" });
  await waitFor(() => expect(container.querySelector(".results")).toBeTruthy());
}

describe("Baseline search results", () => {
  beforeEach(() => {
    bus.reset();
  });

  it("mounts without throwing", async () => {
    const { container } = render(Baseline, { props: { id: "sess", title: "A session · Proj" } });
    // Lands on the browse-root level (index exists, no session view forced).
    await waitFor(() => expect(container.querySelector("header")).toBeTruthy());
  });

  it("guards each_key_duplicate: two hits with the SAME id but different paths both render", async () => {
    const { container } = render(Baseline, { props: { id: "sess", title: "A session · Proj" } });
    await openSearch(container);

    // Same session id can legitimately exist in two different project folders
    // (a workspace copied/moved). Keyed by id alone the list `{#each}` would throw
    // each_key_duplicate and freeze the whole window; the fix keys on
    // `projectPath|id`. Emit exactly that collision.
    bus.emit("search-hit", mkHit({ id: "dup", projectPath: "C:/a/Proj", project: "ProjA", title: "In A", score: 5 }));
    bus.emit("search-hit", mkHit({ id: "dup", projectPath: "C:/b/Proj", project: "ProjB", title: "In B", score: 3 }));
    await tick();

    await waitFor(() => expect(container.querySelectorAll(".hit").length).toBe(2));
    const titles = [...container.querySelectorAll(".htitle")].map((n) => n.textContent?.trim());
    expect(titles).toContain("In A");
    expect(titles).toContain("In B");
  });

  it("dedups a genuine re-emit of the same (path,id), keeping the higher score", async () => {
    const { container } = render(Baseline, { props: { id: "sess", title: "A session · Proj" } });
    await openSearch(container);

    bus.emit("search-hit", mkHit({ id: "dup", projectPath: "C:/a/Proj", title: "first", score: 2 }));
    bus.emit("search-hit", mkHit({ id: "dup", projectPath: "C:/a/Proj", title: "second", score: 9 }));
    await tick();

    // Same compound key -> collapses to one row (the re-emit is not a new hit).
    await waitFor(() => expect(container.querySelectorAll(".hit").length).toBe(1));
    // Higher score wins, so the later, higher-scored payload is the one shown.
    expect(container.querySelector(".htitle")?.textContent?.trim()).toBe("second");
  });
});
