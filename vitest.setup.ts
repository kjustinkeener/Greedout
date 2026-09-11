// Global test setup: jsdom lacks a few browser APIs the app touches at module
// load or on mount. Stub them so components can mount without ReferenceErrors.
// (This file lives outside `src/`, so it is not part of the svelte-check graph.)
import "@testing-library/jest-dom/vitest";
import { vi } from "vitest";

// theme.ts registers a prefers-color-scheme listener at module load, and Settings
// samples matchMedia. jsdom ships neither, so provide a no-op MediaQueryList.
if (!window.matchMedia) {
  window.matchMedia = vi.fn().mockImplementation((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    addListener: vi.fn(),
    removeListener: vi.fn(),
    dispatchEvent: vi.fn(),
  })) as unknown as typeof window.matchMedia;
}

// The Context Explorer treemap observes its container's size. jsdom has no
// ResizeObserver; a no-op keeps the effect from throwing (layout stays 0x0,
// which is fine for the each-key tests, which don't need real geometry).
if (!("ResizeObserver" in globalThis)) {
  class ResizeObserverStub {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (globalThis as any).ResizeObserver = ResizeObserverStub;
}
