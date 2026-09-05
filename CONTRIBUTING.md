# Contributing to Greedout

Thanks for your interest in improving Greedout. It is a Tauri v2 + Svelte 5
desktop app that reads Claude Code transcripts and shows them as live gauges.

## Build and run

Prerequisites: [Rust](https://rustup.rs) (stable), [Node.js](https://nodejs.org)
24 or newer, and the
[Tauri v2 system prerequisites](https://v2.tauri.app/start/prerequisites/).

Node 24 is not optional: the translation gate imports the catalogs as TypeScript
and relies on Node stripping the types, which older releases do not do.

Windows only, deliberately. The app reads a Windows-only transcript layout and
installs itself through Windows APIs, so there is nothing to run elsewhere.

```powershell
npm install
npm run tauri dev   # run the app in dev mode
npm run release     # produce the single self-installing exe
```

## What CI checks

Every push and pull request runs the [CI workflow](.github/workflows/ci.yml) on
Windows. These are the same commands, in the same order. The frontend build is
not optional before the Rust check: the crate embeds `dist/`, so without it the
build script cannot compile.

```powershell
npm run check                                                   # svelte-check plus the translation gate
npm run build
cargo check --manifest-path src-tauri/Cargo.toml --locked --all-targets
```

The translation gate fails on a renamed key and warns on a missing one. If you
add a user-visible string, add it to every catalog under `src/lib/locales/`.

## Pull requests

- Keep changes focused, and say what and why in the PR body.
- Update `README.md` if you change install, build, or configuration behaviour.
- Avoid the em-dash character (U+2014) in committed content; a repo git hook
  blocks it. Use a comma, colon, parentheses, or a spaced hyphen instead.
- Anything touching the updater or the install path deserves an explicit note in
  the PR about what you tested, since neither is covered by CI.

## Reporting bugs

Open an issue with your Windows version, the Greedout version from the About
window, and what you expected instead. For anything security related, see
[SECURITY.md](SECURITY.md) rather than filing an issue.
