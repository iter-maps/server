# 0004 — One gateway binary for all Rust-owned surfaces

- **Status:** Accepted
- **Date:** 2026-06-27
- **Supersedes:** —
- **Superseded by:** —

## Context

An earlier reference implementation split serving across a static file
server and a separate gateway service (different ports). The Rust-owned surfaces
— tiles, styles, glyphs, sprite, overlays, offline, live-trains, health, and the
routing/geocoding proxy — could be one binary or several. They share a config,
an error envelope, and a request profile, and are all stateless; the service
split is not load-bearing.

## Decision

We will serve all Rust-owned surfaces from a single `iter-gateway` binary,
organized as capability modules behind one router. The static `/health` (client
freshness, 5 fields) takes `/health` + `/health.json`; operator diagnostics use
`/livez` + `/readyz`.

## Consequences

- One image, one deployment unit to scale — simplest "clone + up" and simplest
  horizontal scaling for the stateless tier.
- Internal path collisions must be resolved deliberately (done for `/health`).
- If one surface ever needs independent scaling (e.g. offline's heavy extracts),
  it can be split out later; the module boundaries make that mechanical, and such
  a split would get its own ADR.

## Alternatives considered

- **Separate static + gateway binaries** — more deployment units for no benefit
  while the surfaces share everything.
- **A binary per surface** — operational sprawl with no scaling payoff.
