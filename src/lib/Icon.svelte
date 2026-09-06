<script lang="ts" module>
  // One stroked icon set on a single 24 grid, colored by inheritance.
  //
  // Greedout's headline feature is swapping the interface font, and a typed glyph is
  // text: it takes whichever installed font happens to carry that codepoint, so on the
  // shipped default several of these fell through to Segoe UI Symbol and rendered in a
  // different typeface than the label beside them. Picking any display face made it
  // worse. A path costs a line and cannot be reached by the font picker.
  //
  // Geometry follows Lucide (24x24, stroke 2, round caps), so new shapes drop in
  // unmodified. See the shared icon set; add a name to the union, then its `d` strings.
  export type IconName =
    | "x"
    | "check"
    | "refresh"
    | "expand"
    | "chevron-left"
    | "chevron-right"
    | "more"
    | "layout"
    | "sliders"
    | "info"
    | "minus"
    | "home"
    | "search"
    | "bar-chart"
    | "arrow-up";

  const PATHS: Record<IconName, string[]> = {
    x: ["M18 6 6 18", "M6 6l12 12"],
    check: ["M20 6 9 17l-5-5"],
    "arrow-up": ["M12 19V5", "M5 12l7-7 7 7"],
    refresh: ["M21 12a9 9 0 1 1-2.64-6.36L21 8", "M21 3v5h-5"],
    expand: ["M15 3h6v6", "M9 21H3v-6", "M21 3l-7 7", "M3 21l7-7"],
    "chevron-left": ["M15 18l-6-6 6-6"],
    "chevron-right": ["M9 18l6-6-6-6"],
    // Zero-length paths, drawn only because the caps are round.
    more: ["M5 12h.01", "M12 12h.01", "M19 12h.01"],
    layout: ["M3 3h7v9H3z", "M14 3h7v5h-7z", "M14 12h7v9h-7z", "M3 16h7v5H3z"],
    sliders: [
      "M21 4h-7", "M10 4H3", "M21 12h-9", "M8 12H3", "M21 20h-5", "M12 20H3",
      "M14 2v4", "M8 10v4", "M16 18v4",
    ],
    info: ["M22 12a10 10 0 1 1-20 0 10 10 0 1 1 20 0", "M12 16v-4", "M12 8h.01"],
    minus: ["M5 12h14"],
    home: ["M3 9l9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z", "M9 22V12h6v10"],
    search: ["M18 11a7 7 0 1 1-14 0 7 7 0 1 1 14 0", "M16.5 16.5 21 21"],
    "bar-chart": ["M18 20V10", "M12 20V4", "M6 20v-6", "M3 20h18"],
  };
</script>

<script lang="ts">
  interface Props {
    name: IconName;
    /** Rendered size in px. The grid is always 24; this only scales it. */
    size?: number;
    /** Stroke weight in grid units, so it scales with `size` like the shape does. */
    width?: number;
  }
  let { name, size = 13, width = 2 }: Props = $props();
</script>

<svg
  viewBox="0 0 24 24"
  width={size}
  height={size}
  fill="none"
  stroke="currentColor"
  stroke-width={width}
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  {#each PATHS[name] as d}
    <path {d} />
  {/each}
</svg>

<style>
  /* An svg is a replaced element and would sit on the bottom edge of the line box,
     about 2px below where the glyph it replaces used to sit. Taking it out of the line
     box entirely is what removes that, and the collapsing whitespace with it. */
  svg {
    display: block;
    flex: none;
  }
</style>
