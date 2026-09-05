# Security policy

## Reporting a vulnerability

Please report privately, not in a public issue. Use GitHub's
[private vulnerability reporting](https://github.com/kjustinkeener/Greedout/security/advisories/new)
form on this repository, which is visible only to the maintainer.

Greedout is maintained by one person, so replies are best effort rather than a
guaranteed window. You will get an acknowledgement, and a fix or an explanation
of why something is not a vulnerability.

Supported version: the latest release. Fixes go into a new release rather than
patches to older ones.

## The updater, and the signing key

Greedout updates itself. The public half of a minisign key is compiled into the
binary (`src-tauri/src/update.rs`), and a downloaded update is verified against
it before anything touches disk. An update that fails verification is discarded,
so a compromised download host on its own cannot install code.

That makes the private key the thing worth attacking. If it is ever exposed, an
already-installed copy cannot be rescued by a new release, because it will only
accept builds signed by the key it was compiled with. The recovery is a new key,
a new build carrying it, and a notice here, on the releases page, and at
[fasterdb.com/software/greedout](https://fasterdb.com/software/greedout/). That
site is the out-of-band channel: if the in-app updater is the thing that is broken
or untrusted, it is where the notice will say so, and the replacement is installed
by hand from the releases page. If you believe the key has leaked, report it
through the form above and say so plainly, since it is the highest severity
report this project can receive.

Release binaries are published only after their signature is verified against
that same public key with `tools/verify-release`, which is why releases are
built as drafts first.
