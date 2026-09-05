<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import Icon from "./Icon.svelte";
  import Speedo from "./Speedo.svelte";
  import { THEMES, gaugeColor, previewTheme, themeTick } from "./theme";
  import { t } from "./i18n.svelte";

  let {
    version,
    installDir,
    existing,
  }: { version: string; installDir: string; existing: boolean } = $props();

  let desktopShortcut = $state(true);
  let phase = $state<"idle" | "working" | "done" | "error">("idle");
  let error = $state("");
  let built = $state("");

  // The window is transparent so the card can have its own rounded frame, but
  // app.css paints a solid body behind it. Clear it for this window only, and
  // put it back on teardown so nothing leaks into a later mount.
  onMount(() => {
    const targets = [document.documentElement, document.body, document.getElementById("app")!];
    const prev = targets.map((el) => el.style.background);
    targets.forEach((el) => (el.style.background = "transparent"));
    void invoke<number>("build_epoch")
      .then((secs) => {
        if (secs) built = new Date(secs * 1000).toISOString().slice(0, 10);
      })
      .catch(() => {});
    return () => targets.forEach((el, i) => (el.style.background = prev[i]));
  });

  // The hero is the app's own gauge, not a decoration invented for the card:
  // whoever is installing this is about to spend their time looking at it.
  const SITE = "https://fasterdb.com/software/greedout/";
  const TARGET = 200_000;
  const IDLE = 0; // the needle rests on the peg, like a gauge with the key out
  let pct = $state(IDLE);
  let ctx = $derived(Math.round(pct * TARGET));
  const fmtK = (n: number | null) => (n == null ? "--" : `${Math.round(n / 1000)}k`);

  // The gauge is a throttle, not a cutscene, and the needle has mass. It is a
  // second-order system: holding applies torque, drag sets where the sweep tops
  // out, and releasing hands it to a spring back to idle. Nothing eases toward a
  // target, so the needle overshoots the stop, bounces, and settles the way a
  // real one does. A tap barely lifts it; hold and it keeps winding on.
  const MAXP = 1.32; // the needle's stop, well past redline
  // Tuned by simulating the hold. Two torque terms, because the low end and the
  // high end are separate feelings: the main curve owns everything above the
  // first third and is what makes the top expensive, and BOOST is a low-end
  // shove that is already zero by the time the needle gets anywhere the high end
  // cares about. So the gauge leaps off the peg (a fifth of the dial in a third
  // of a second, half of it in one) while thirty seconds still only buys 90% and
  // redline is still a minute and a half away.
  const TORQUE = 1.0; // pct/s^2 while held, at the bottom of the sweep
  const BOOST = 40.0; // extra torque off the peg; big, because drag eats it fast
  const BOOST_END = 0.45; // fraction of the sweep the boost has faded out by
  const FALLOFF = 7.0; // exponent on the torque curve's collapse toward the stop
  const DRAG = 4.2; // linear, dominates at low revs
  const DRAG2 = 45.0; // square of speed, and the reason a fast climb cannot last
  const RETURN = 4.2; // spring constant of the return-to-idle
  const DAMP = 1.9; // under critical, so it undershoots idle and comes back
  const STOP_BOUNCE = 0.35; // energy kept when the needle hits either stop

  let held = $state(false);
  let base = IDLE; // the needle itself; `pct` is it plus flutter
  let vel = 0; // pct per second
  let raf = 0;
  let last = 0;
  let revving = $derived(pct > IDLE + 0.05);

  // The install button rides the needle up the gauge's own cyan-to-red scale, so
  // the one control that matters is lit by the toy above it. `$themeTick` is read
  // so a reskin recolors the button even while the needle is sitting still.
  let ctaColor = $derived.by(() => {
    void $themeTick;
    return gaugeColor(Math.min(1, pct));
  });

  function loop(now: number) {
    // Clamped: a backgrounded window hands back a gap of seconds, which would
    // integrate into a needle teleporting off the dial on the first frame back.
    const dt = Math.min(0.04, (now - last) / 1000);
    last = now;

    // Redline is hard to reach, the way it is in a car. Torque follows a curve
    // that collapses as the needle climbs, and drag is mostly the square of speed,
    // so the bottom of the sweep is eager and the top is a creep you have to keep
    // holding for. The last few percent are asymptotic: they are the reward for
    // holding on well past the point where most people let go.
    const rel = Math.min(1, Math.max(0, (base - IDLE) / (MAXP - IDLE)));
    const curve = Math.pow(Math.max(0, 1 - rel * rel), FALLOFF);
    const boost = BOOST * Math.pow(Math.max(0, 1 - rel / BOOST_END), 2);
    const acc = held
      ? TORQUE * curve + boost - DRAG * vel - DRAG2 * vel * Math.abs(vel)
      : -RETURN * (base - IDLE) - DAMP * vel;
    vel += acc * dt;
    base += vel * dt;

    // Both ends of the sweep are physical stops, so hitting one is a bounce and
    // not a clamp. The idle stop is what makes a hard release kick and rebound.
    if (base > MAXP) {
      base = MAXP;
      vel = -Math.abs(vel) * STOP_BOUNCE;
    } else if (base < IDLE) {
      base = IDLE;
      vel = Math.abs(vel) * STOP_BOUNCE;
    }

    // Flutter scales with revs. A needle pinned perfectly still at the top is the
    // tell that nothing is actually being simulated.
    const f = Math.max(0, (base - IDLE) / (MAXP - IDLE));
    pct = base + (Math.sin(now / 37) * 0.007 + Math.sin(now / 17) * 0.004) * f;

    if (held || base > IDLE + 0.002 || Math.abs(vel) > 0.01) {
      raf = requestAnimationFrame(loop);
    } else {
      base = IDLE;
      vel = 0;
      pct = IDLE;
      raf = 0;
    }
  }

  /** Idempotent: the loop runs itself down, so anything that changes `held` just
   *  makes sure it is running. */
  function spin() {
    if (raf) return;
    last = performance.now();
    raf = requestAnimationFrame(loop);
  }

  /** The pedal is the pointer: resting on the gauge is the throttle, and moving
   *  off it lifts. Keyboard focus counts as resting on it. */
  function hold() {
    if (held) return;
    held = true;
    spin();
  }

  /** A click, and only a click, is a different palette. The app ships 68 themes
   *  and the picker is three windows deep from here, so this is the one moment a
   *  new user finds out they exist. Hovering must not do it: a cursor crossing
   *  the card would repaint the whole thing on the way past. */
  function shuffleTheme() {
    const pick = THEMES[Math.floor(Math.random() * THEMES.length)];
    // Deliberately not persisted: the card has no config of its own to correct
    // it, so a remembered shuffle would still be there on the next launch and
    // read as the app picking a palette at random.
    if (pick) previewTheme(pick.id);
  }

  function release() {
    if (!held) return;
    held = false;
    spin();
  }


  onDestroy(() => cancelAnimationFrame(raf));

  async function install() {
    if (phase === "working") return; // re-entrancy: the button stays mounted
    phase = "working";
    error = "";
    hold(); // the needle stays up while the work runs, and drops when it lands
    try {
      const exe = await invoke<string>("perform_install", { desktopShortcut });
      release();
      phase = "done";
      // A beat, so "Installed" is readable before the window disappears. The
      // handoff is instant otherwise and the card just blinks out of existence.
      setTimeout(() => void invoke("launch_installed_and_exit", { exe }), 700);
    } catch (e) {
      // Staying on the card with the reason beats vanishing: the failures here
      // are things the user can act on (a locked file, a full disk).
      release();
      error = t("install.failed", { error: String(e) });
      phase = "error";
    }
  }
</script>

<div class="wrap" data-tauri-drag-region>
  <div class="card" class:busy={phase === "working"} data-tauri-drag-region>
    <button
      class="close"
      onclick={() => void getCurrentWindow().close()}
      title={t("win.close")}
      aria-label={t("win.close")}
    >
      <Icon name="x" size={13} />
    </button>

    <!-- pointer-events off so the whole header area drags the window; the gauge
         turns them back on for itself. -->
    <div class="top">
      <button
        class="art"
        class:revving
        onclick={shuffleTheme}
        onpointerenter={hold}
        onpointerleave={release}
        onpointercancel={release}
        onfocus={hold}
        onblur={release}
        aria-label={t("install.pokeTip")}
      >
        <Speedo {pct} {ctx} target={TARGET} {fmtK} />
      </button>
      <span class="brand">greedout</span>
      <p class="tag">{t("about.tagline")}</p>
    </div>

    {#if phase === "done"}
      <p class="state ok"><Icon name="check" size={14} /> {t("install.starting")}</p>
    {:else}
      {#if phase === "error"}
        <p class="state err">{error}</p>
      {/if}
      <button
        class="cta"
        style="--cta: {ctaColor}"
        onclick={install}
        disabled={phase === "working"}
      >
        {#if phase === "working"}
          {t("install.working")}
        {:else if phase === "error"}
          {t("install.retry")}
        {:else if existing}
          {t("install.update")}
        {:else}
          {t("install.heading")}
        {/if}
      </button>
      <label class="opt">
        <input type="checkbox" bind:checked={desktopShortcut} disabled={phase === "working"} />
        <span>{t("install.desktopShortcut")}</span>
      </label>
    {/if}

    <div class="foot">
      <span class="fver">{t("about.version", { version })}{built ? ` · ${built}` : ""}</span>
      <span class="path" title={installDir}>{t("install.location")}: {installDir}</span>
      <button class="site" onclick={() => void invoke("open_url", { url: SITE })}>
        {SITE.replace("https://", "").replace(/\/$/, "")}
      </button>
    </div>
  </div>
</div>

<style>
  /* Chrome, not a document. A frameless card that selects text on a drag and
     highlights words on a double click reads as a broken web page. */
  .wrap {
    width: 100vw;
    height: 100vh;
    background: transparent;
    user-select: none;
    cursor: default;
  }
  .card {
    position: absolute;
    inset: 0;
    box-sizing: border-box;
    padding: 30px 34px 52px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
    color: var(--fg);
    /* The glow is the gauge's own cyan, so the card recolors with every theme
       the gauge is poked into. */
    background:
      radial-gradient(120% 70% at 50% -8%, color-mix(in srgb, var(--g0) 22%, transparent), transparent 62%),
      rgb(var(--bg-rgb));
    border: 1px solid var(--edge);
    border-radius: 8px;
    /* The poked palettes push the glow past the edge; clip it rather than scroll. */
    overflow: hidden;
    transition: background 0.35s ease;
  }
  .close {
    position: absolute;
    top: 8px;
    right: 8px;
    z-index: 2;
    width: 26px;
    height: 26px;
    display: grid;
    place-items: center;
    padding: 0;
    border: none;
    border-radius: 7px;
    background: transparent;
    color: var(--muted);
    cursor: pointer;
    transition:
      background 0.12s ease,
      color 0.12s ease;
  }
  .close:hover {
    background: var(--panel);
    color: var(--fg);
  }
  .top {
    pointer-events: none;
    width: 100%;
  }
  .art {
    pointer-events: auto;
    display: block;
    width: 132px;
    margin: 0 auto 2px;
    padding: 0;
    background: none;
    border: none;
    cursor: pointer;
    filter: drop-shadow(0 0 14px color-mix(in srgb, var(--g0) 30%, transparent));
    transition:
      filter 0.3s ease,
      transform 0.1s ease;
  }
  .art:active {
    transform: scale(0.97);
  }
  .art.revving {
    filter: drop-shadow(0 0 26px color-mix(in srgb, var(--g1) 65%, transparent));
  }
  /* The wordmark, drawn the way the app's own header draws it: fixed face and
     weight (exempt from the font preferences, because it is a mark and not
     interface text) and filled with the gauge gradient, so it reskins with
     everything else on the card. */
  .brand {
    display: block;
    font-family: "Doto", ui-monospace, monospace;
    font-size: 34px;
    font-size-adjust: none;
    font-weight: 900;
    line-height: 1.2;
    letter-spacing: 0.01em;
    text-transform: lowercase;
    background: linear-gradient(0deg, var(--g0), var(--g1) 52%, var(--g2));
    -webkit-background-clip: text;
    background-clip: text;
    -webkit-text-fill-color: transparent;
    color: transparent;
    filter: drop-shadow(0 1px 1px rgba(0, 0, 0, 0.85));
  }
  .tag {
    margin: 6px 0 22px;
    color: var(--muted);
    line-height: 1.45;
  }
  .cta {
    width: 100%;
    padding: 11px 18px;
    font: inherit;
    font-size: 15px;
    font-size-adjust: var(--font-ui-adj);
    font-weight: var(--w-semibold);
    color: rgb(var(--bg-rgb));
    background: linear-gradient(180deg, color-mix(in srgb, var(--cta) 88%, white), var(--cta));
    border: none;
    border-radius: 10px;
    cursor: pointer;
    box-shadow: 0 8px 22px color-mix(in srgb, var(--cta) 35%, transparent);
    transition:
      transform 0.08s ease,
      box-shadow 0.15s ease,
      filter 0.15s ease;
  }
  .cta:hover:not(:disabled) {
    filter: brightness(1.06);
    box-shadow: 0 10px 26px color-mix(in srgb, var(--cta) 50%, transparent);
  }
  .cta:active:not(:disabled) {
    transform: translateY(1px);
  }
  .cta:disabled {
    cursor: default;
    opacity: 0.8;
  }
  .opt {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    margin-top: 13px;
    color: var(--muted);
    cursor: pointer;
  }
  .opt input {
    accent-color: var(--g0);
    cursor: pointer;
  }
  .opt input:disabled {
    cursor: default;
  }
  .state {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    margin: 0 0 12px;
  }
  .state.ok {
    color: var(--g0);
    font-weight: var(--w-semibold);
    margin-bottom: 0;
  }
  .state.err {
    color: var(--red);
  }
  .foot {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 12px;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    padding: 0 16px;
    font-size: 11px;
    color: var(--muted);
    opacity: 0.75;
  }
  .fver {
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
  }
  .site {
    padding: 0;
    border: none;
    background: none;
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
    color: var(--g0);
    opacity: 0.9;
    cursor: pointer;
  }
  .site:hover {
    opacity: 1;
    text-decoration: underline;
  }
  /* Full path in the tooltip: it is longer than the card on most machines, and
     truncating it without a way to read it is worse than not showing it. */
  .path {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
