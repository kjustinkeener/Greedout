<script lang="ts">
  // Inlined brand mark so the inner speedo dial can be themed: its gauge sweep
  // reads the live --g0/g1/g2 vars (same cyan→yellow→red scale as the gauges).
  // The shield keeps its static brand blue→green / gray. `size` in px.
  let { size = 16 }: { size?: number } = $props();
</script>

<svg
  class="brand-icon"
  width={size}
  height={size}
  viewBox="-64 -64 400 400"
  aria-hidden="true"
  xmlns="http://www.w3.org/2000/svg"
>
  <defs>
    <linearGradient id="bi-shield-fill" x1="266.52" y1="297.73" x2="5.48" y2="-23.92" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#2a2a2a" />
      <stop offset="1" stop-color="#969696" />
    </linearGradient>
    <linearGradient id="bi-shield-stroke" x1="135.29" y1="297.73" x2="135.29" y2="-23.92" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#4879b9" />
      <stop offset="1" stop-color="#40b951" />
    </linearGradient>
    <linearGradient id="bi-gauge" x1="45.77" y1="110.89" x2="226.23" y2="110.89" gradientUnits="userSpaceOnUse">
      <stop class="g0" offset="0" />
      <stop class="g1" offset="0.52" />
      <stop class="g2" offset="1" />
    </linearGradient>
  </defs>
  <!-- shield (annulus) -->
  <path
    d="m 136,-14.594398 121.19883,41.95344 V 134.57339 C 257.19883,213.81878 205.9224,260.43372 136,288.40268 66.077595,260.43372 14.801165,213.81878 14.801165,134.57339 V 27.359042 Z"
    fill="url(#bi-shield-fill)"
    stroke="url(#bi-shield-stroke)"
    stroke-width="30"
    stroke-linejoin="miter"
  />
  <!-- speedo dial (themed gauge sweep) -->
  <path
    d="M 135.84961 46.670898 A 90.233145 90.233145 0 0 0 53.995117 174.54883 L 92.354492 156.5166 A 48.124344 48.124344 0 0 1 136.15039 88.780273 L 136.45117 88.780273 A 48.124344 48.124344 0 0 1 180.14258 156.75098 L 218.00488 174.54883 A 90.233145 90.233145 0 0 0 136.15039 46.670898 L 135.84961 46.670898 z"
    fill="url(#bi-gauge)"
  />
  <!-- needle -->
  <path d="M 131.94788,144.17974 69.45794,89.26279 141.27086,130.19526 Z" fill="#e6edf3" />
  <!-- hub -->
  <circle cx="136.11653" cy="136.90419" r="9.3229866" fill="#3fb950" />
</svg>

<style>
  .brand-icon {
    display: block;
    flex: 0 0 auto;
    pointer-events: none;
  }
  /* Themeable gauge sweep: recolors with the active theme's gauge scale. */
  .brand-icon .g0 {
    stop-color: var(--g0);
  }
  .brand-icon .g1 {
    stop-color: var(--g1);
  }
  .brand-icon .g2 {
    stop-color: var(--g2);
  }
</style>
