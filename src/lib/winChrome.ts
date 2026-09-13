import { getCurrentWindow } from "@tauri-apps/api/window";

// Track the current window's maximized state so a custom title-bar button can
// swap between the maximize and restore-down glyphs (App-Patterns Title-Bar).
// Call it inside a $effect and run the returned cleanup on teardown. It seeds
// the state once, then follows every resize (OS maximize, snap, our own
// toggleMaximize) so the glyph stays in sync however the window changed.
export function watchMaximized(set: (m: boolean) => void): () => void {
  const w = getCurrentWindow();
  let un: (() => void) | undefined;
  let dead = false;
  const sync = () => w.isMaximized().then((m) => !dead && set(m));
  sync();
  w.onResized(sync).then((u) => (dead ? u() : (un = u)));
  return () => {
    dead = true;
    un?.();
  };
}
