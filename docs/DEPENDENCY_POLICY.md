# Dependency and supply-chain policy

Echo evaluates the complete Windows, Linux, and macOS release graph with
Cargo Deny. `Cargo.lock` is authoritative for builds, unexcepted advisories and
yanked releases fail the gate, registries and Git origins are allow-listed,
and licenses are checked against `deny.toml`.

Run the release dependency gate with the repository-pinned toolchain:

```powershell
cargo deny --locked --all-features check advisories licenses bans sources
```

The gate deliberately allows duplicate versions as warnings. Resolving every
duplicate can force unrelated upstream major-version migrations and is not a
release blocker by itself. New yanked releases, vulnerabilities, unmaintained
crates, unknown sources, and unapproved licenses remain failures.

## Temporary RustSec exceptions

All exceptions below report **no safe upgrade**. They remain visible in the
repository policy and must be reviewed before each release. An exception must
be removed as soon as its exit condition is available; adding another
exception requires an explicit reason and exit condition in both this file and
`deny.toml`.

| Advisory | Locked path | Why it remains | Exit condition |
|---|---|---|---|
| `RUSTSEC-2024-0436` | GPUI Metal -> `paste 1.0.15` | The GPUI fork's image codec trim removes the image-crate path; the remaining macOS-only path is upstream/transitive and has no safe in-range replacement. | Adopt an upstream GPUI/Metal replacement without reducing macOS behavior. |
| `RUSTSEC-2025-0134` | GPUI `reqwest_client` -> `zed-reqwest` -> `rustls-pemfile 2.2.0` | The crate is a transitive compatibility wrapper with no safe upgrade. | Complete the planned reqwest client migration to `rustls-pki-types`-based PEM handling. |
| `RUSTSEC-2026-0192` | GPUI `fontdb`/`usvg`/`rustybuzz` -> `ttf-parser 0.25.1` | The rendering stack owns this parser and no safe upgrade exists. | Take upstream's `skrifa` migration after validating that shaping and SVG/font rendering are unchanged. |
| `RUSTSEC-2026-0206` | GPUI -> `usvg 0.46.0` -> `rustybuzz 0.20.1` | RustSec classifies `rustybuzz` as unmaintained; the pinned GPUI SVG stack has no safe upgrade and Echo does not depend on it directly. | Update GPUI's `usvg`/`resvg` stack once it no longer depends on `rustybuzz`, then revalidate SVG emotes and image-backed 7TV name paints before removing the exception. |
| `RUSTSEC-2026-0370` | GPUI accesskit/atspi/oo7 and `notify-rust` -> `zbus 5` -> `zvariant 5` | `zbus 6` absorbed the fix, but GPUI's Linux stack and `notify-rust` still use `zbus 5`. | Move to `zbus 6` once those dependencies do, together with Echo's Linux KWin client (`src/utils/kwin.rs`). |
| `RUSTSEC-2026-0371` | `zbus 5` -> `zbus_names 4` | Same `zbus 5` path as `RUSTSEC-2026-0370`. | The same `zbus 6` migration. |
| `RUSTSEC-2026-0372` | `zbus 5` -> `zvariant_derive 5` | Same `zbus 5` path as `RUSTSEC-2026-0370`. | The same `zbus 6` migration. |

Postcard is the sole persistence writer and primary reader; the abandoned
`bincode` package is absent from the lockfile. The maintained, wire-compatible
`cu-bincode` fork is restricted to bounded, exact-consumption, decode-only
legacy DTOs so existing history and cache snapshots remain readable.

The GPUI fork's image codec trim keeps every format referenced by GPUI's
compiled image and clipboard paths while removing the unused AVIF encoder, DDS,
EXR, HDR, QOI, TGA, Rayon, and their transitive graph.
`cargo tree --locked --target all -i ravif` is empty, and `paste` is reachable
only through the macOS Metal dependency.

## Git sources

Echo uses only crates.io plus the small allow-list of upstream Git repositories
in `deny.toml` (`[sources]`). The Git dependencies come from the pinned GPUI/Zed
workspace, and the GPUI fork itself is reconstructed from the pinned Zed
revision and the patch series described in `docs/GPUI_FORK.md`. Every Git edge
is pinned to an immutable revision in `Cargo.lock`.
