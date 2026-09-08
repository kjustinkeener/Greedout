# Third-party notices

Greedout bundles and redistributes the third-party components listed below. This
file exists because some of those licences require their notices to travel with
the binary, not merely with the source.

Greedout's own code is MIT licensed; see [LICENSE](LICENSE).

## Bundled fonts

Every face in the font picker's built-in catalog is redistributed inside the
installer as a WOFF2 file. All of them are licensed under the **SIL Open Font
License, Version 1.1**, whose full text is at <https://openfontlicense.org/>.
Copyright is held by each family's respective authors; the per-family notice
ships in the corresponding npm package and is reproduced in
`node_modules/<package>/LICENSE` in a source checkout.

The OFL permits this redistribution, bundled and embedded, provided the fonts
are not sold on their own and the licence travels with them. Greedout does not
rename any face.

| Family | Package |
|---|---|
| Archivo | `@fontsource-variable/archivo` |
| Audiowide | `@fontsource/audiowide` |
| Azeret Mono | `@fontsource-variable/azeret-mono` |
| B612 | `@fontsource/b612` |
| B612 Mono | `@fontsource/b612-mono` |
| Barlow Semi Condensed | `@fontsource/barlow-semi-condensed` |
| Bebas Neue | `@fontsource/bebas-neue` |
| Big Shoulders Display | `@fontsource-variable/big-shoulders-display` |
| Chakra Petch | `@fontsource/chakra-petch` |
| DM Mono | `@fontsource/dm-mono` |
| Doto | `@fontsource/doto` |
| Exo 2 | `@fontsource-variable/exo-2` |
| Figtree | `@fontsource-variable/figtree` |
| Fira Code | `@fontsource-variable/fira-code` |
| Geist Mono | `@fontsource-variable/geist-mono` |
| Handjet | `@fontsource-variable/handjet` |
| IBM Plex Mono | `@fontsource/ibm-plex-mono` |
| IBM Plex Sans | `@fontsource-variable/ibm-plex-sans` |
| Inter | `@fontsource-variable/inter` |
| JetBrains Mono | `@fontsource-variable/jetbrains-mono` |
| Jura | `@fontsource-variable/jura` |
| Kode Mono | `@fontsource-variable/kode-mono` |
| Major Mono Display | `@fontsource/major-mono-display` |
| Manrope | `@fontsource-variable/manrope` |
| Martian Mono | `@fontsource-variable/martian-mono` |
| Michroma | `@fontsource/michroma` |
| Nova Mono | `@fontsource/nova-mono` |
| Orbitron | `@fontsource-variable/orbitron` |
| Oxanium | `@fontsource-variable/oxanium` |
| Pixelify Sans | `@fontsource-variable/pixelify-sans` |
| Rajdhani | `@fontsource/rajdhani` |
| Red Hat Mono | `@fontsource-variable/red-hat-mono` |
| Roboto Condensed | `@fontsource-variable/roboto-condensed` |
| Roboto Mono | `@fontsource-variable/roboto-mono` |
| Saira | `@fontsource-variable/saira` |
| Share Tech Mono | `@fontsource/share-tech-mono` |
| Silkscreen | `@fontsource/silkscreen` |
| Sono | `@fontsource-variable/sono` |
| Sora | `@fontsource-variable/sora` |
| Source Code Pro | `@fontsource-variable/source-code-pro` |
| Space Grotesk | `@fontsource-variable/space-grotesk` |
| Space Mono | `@fontsource/space-mono` |
| Teko | `@fontsource-variable/teko` |
| Tektur | `@fontsource-variable/tektur` |
| Titillium Web | `@fontsource/titillium-web` |
| VT323 | `@fontsource/vt323` |
| Workbench | `@fontsource-variable/workbench` |

48 families in total, all obtained through the
[Fontsource](https://fontsource.org/) project.

Fonts already installed on the machine can also be selected. Those are read from
the system, never redistributed, and are not covered by this notice.

## Bundled SQLite

The optional session cache uses SQLite, statically compiled into the binary via
the `rusqlite` crate's `bundled` feature. SQLite is in the **public domain**; see
<https://www.sqlite.org/copyright.html>.

## Rust dependencies

Greedout ships as a single self-contained `greedout.exe`, so every crate in the
normal dependency graph is statically linked into the binary that reaches you.
The direct dependencies declared in `src-tauri/Cargo.toml` are:

| Crate | Purpose | Licence |
|---|---|---|
| `tauri` | Application shell and webview host | MIT OR Apache-2.0 |
| `tauri-plugin-single-instance` | Focuses the running window instead of starting a second one | MIT OR Apache-2.0 |
| `tauri-build` | Build-time only, not linked | MIT OR Apache-2.0 |
| `serde`, `serde_json` | Transcript and config parsing | MIT OR Apache-2.0 |
| `glob` | Session sidecar discovery | MIT OR Apache-2.0 |
| `dirs` | Locating the user's config directories | MIT OR Apache-2.0 |
| `rusqlite` | Optional session cache (see the SQLite note above) | MIT |
| `sysinfo` | Status bar CPU and memory sampling | MIT |
| `base64` | Decoding the update manifest's signature | MIT OR Apache-2.0 |
| `ureq` | HTTPS fetch of the update manifest and payload | MIT OR Apache-2.0 |
| `minisign-verify` | Verifying the update signature before anything touches disk | MIT |
| `semver` | Update version comparison | MIT OR Apache-2.0 |
| `winreg` (Windows only) | Installed-font enumeration from the Fonts registry key | MIT |

Those pull in roughly 230 further crates. Enumerating all of them here would be
noise, but claiming they are uniformly MIT or Apache-2.0 would be untrue, so the
exceptions are named individually below. To regenerate the full picture, run
this in `src-tauri/`:

```
cargo tree --locked -e normal --target x86_64-pc-windows-msvc --format "{p}|{l}"
```

`cargo about` will produce a complete machine-generated manifest from the same
graph.

### Licences other than MIT, Apache-2.0 and BSD

Every crate listed here is redistributed inside `greedout.exe` unless noted.

- **`ring` 0.17 (Apache-2.0 AND ISC)**, reached through `ureq` and `rustls`.
  This is the one dependency with a bespoke notice requirement; its text is
  reproduced in full in the next section.
- **`untrusted` 0.9 (ISC)** and **`rustls-webpki` 0.103 (ISC)**, the parsing
  layer beneath `ring`. `rustls` itself is Apache-2.0 OR ISC OR MIT.
- **`webpki-roots` 0.26 and 1.0 (CDLA-Permissive-2.0)**, which embed Mozilla's
  root CA bundle so the updater can validate TLS without touching the Windows
  certificate store. CDLA-Permissive-2.0 is a permissive data licence and
  requires only that the licence text travel with the data; the bundle itself is
  Mozilla's, published as part of the NSS project.
- **`option-ext` 0.2 (MPL-2.0)**, reached through `dirs`. MPL-2.0 is file-level
  copyleft: the crate is used unmodified, and its source is available at
  <https://github.com/soc/option-ext>. Three further MPL-2.0 crates
  (`cssparser`, `selectors`, `dtoa-short`) appear in the graph but only inside
  Tauri's `tauri-macros` proc macro, so they run at compile time and are not
  present in the shipped binary.
- **The ICU crates (Unicode-3.0)**: `icu_collections`, `icu_locale_core`,
  `icu_normalizer`, `icu_normalizer_data`, `icu_properties`,
  `icu_properties_data`, `icu_provider`, `litemap`, `potential_utf`, `tinystr`,
  `writeable`, `yoke`, `zerofrom`, `zerotrie`, `zerovec` (plus the matching
  derive macros), and `unicode-ident` under `(MIT OR Apache-2.0) AND
  Unicode-3.0`. Unicode-3.0 is the Unicode License v3, which permits
  redistribution provided the notice and Unicode data terms are kept; see
  <https://www.unicode.org/license.txt>.
- **`tao` 0.35 (Apache-2.0 only)** and **`dpi` 0.1 (Apache-2.0 AND MIT)**,
  Tauri's window layer. Apache-2.0 requires that its NOTICE conditions be
  preserved; no NOTICE file is shipped by either crate.
- **`subtle` 2.6, `alloc-no-stdlib`, `alloc-stdlib` (BSD-3-Clause)**;
  `brotli` (BSD-3-Clause AND MIT) and `brotli-decompressor` (BSD-3-Clause OR
  MIT) are compile-time only, via `tauri-codegen`.
- **`foldhash` 0.2 (Zlib)**, compile-time only via `tauri-utils`;
  `miniz_oxide` (MIT OR Zlib OR Apache-2.0) and `raw-window-handle`
  (MIT OR Apache-2.0 OR Zlib) are linked but taken under their MIT option.
- **`adler2` (0BSD OR MIT OR Apache-2.0)**, **`dunce` (CC0-1.0 OR MIT-0 OR
  Apache-2.0)**, **`zerocopy` (BSD-2-Clause OR Apache-2.0 OR MIT)** and the
  `Unlicense OR MIT` crates (`aho-corasick`, `byteorder`, `memchr`, `same-file`,
  `walkdir`, `winapi-util`) are all taken under their MIT option.

### Notice required by *ring*

*ring* uses an "ISC" style licence for its own code and the Apache License 2.0
for code sourced from BoringSSL. The ISC portion reads, in full:

```
Copyright 2015-2025 Brian Smith.

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY
SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION
OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN
CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
```

Code that *ring* sourced from BoringSSL is under the Apache License 2.0, whose
text is at <https://www.apache.org/licenses/LICENSE-2.0>; the per-file licence
attribution is recorded at the top of each such file, and the accompanying
`LICENSE-BoringSSL` file (which carries the original OpenSSL and SSLeay
copyright notices) ships in the `ring` crate source at
<https://github.com/briansmith/ring>. Small portions polyfilled from the
`once_cell` project are dual MIT/Apache-2.0.

## JavaScript dependencies

`@tauri-apps/api` (MIT/Apache-2.0) is the only non-font runtime dependency.
Build-time only, and therefore not redistributed: Svelte, Vite, TypeScript,
`svelte-check`, and the Tauri CLI, all MIT.
