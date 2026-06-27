# 0005 — Layered licensing, REUSE, and DCO

- **Status:** Accepted
- **Date:** 2026-06-27
- **Supersedes:** —
- **Superseded by:** —

## Context

The backend runs as a network service; the project also ships docs and (in
sibling repos) apps and infra. A single license fits none of these well: a
hosted service can be modified without distribution (the GPL "SaaS loophole"),
while docs and infra want maximal reuse. The repo therefore mixes licenses by
layer and needs that to be machine-auditable. Contributions need a low-friction
provenance mechanism.

## Decision

We will license by layer: **AGPL-3.0-or-later** for code (network copyleft closes
the SaaS loophole), **CC-BY-4.0** for docs. License texts live in `LICENSES/` and
file licensing is declared in `REUSE.toml` (REUSE spec). Contributions are taken
under the **DCO** (`Signed-off-by`, inbound=outbound), **not** a CLA.

## Consequences

- The multi-license repo stays auditable (`reuse lint` in CI).
- DCO keeps contribution friction low but forecloses later relicensing without
  every contributor's agreement — accepted; there is no dual-licensing ambition.
- The app/infra layers (MPL/Apache for iOS, GPL for Android, Apache for infra)
  live in sibling repos and are out of scope here.

## Alternatives considered

- **One license for everything** — either too permissive for the service or too
  restrictive for docs/infra.
- **A CLA** — enables relicensing but adds friction and centralizes rights; not
  wanted.
