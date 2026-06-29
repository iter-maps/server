# 0007 — Offline extraction via the go-pmtiles CLI

- **Status:** Accepted
- **Date:** 2026-06-27
- **Supersedes:** —
- **Superseded by:** —

## Context

Offline pre-download returns a bbox-clipped PMTiles archive. Because the source
archive is clustered, the clip is a cheap byte-range read — no tile re-render.
Two ways to perform it: shell out to the pinned `go-pmtiles` CLI, or implement a
PMTiles v3 reader/extractor in Rust (a future `iter-pmtiles` crate). The CLI is
mature and exact, and a server-side range-extract via `go-pmtiles` is the proven
approach.

## Decision

We will shell out to the pinned `go-pmtiles` CLI for `/offline/extract`, run as a
subprocess against the clustered source, gated by a no-queue concurrency
semaphore. The binary ships in the image; a missing binary fails gracefully with
`EXTRACT_FAILED`. A Rust `iter-pmtiles` reader is deferred.

## Consequences

- A runtime dependency on the `go-pmtiles` binary being present in the serving
  image — must be added to the gateway Dockerfile.
- The extract path is not unit-testable without the binary; the validation, caps,
  and concurrency gate (the abuse protection that actually matters on this
  public surface) are pure and fully tested.
- If a Rust PMTiles reader is later wanted (to drop the external dep, or to share
  code with tile serving), it gets its own ADR superseding this.

## Alternatives considered

- **Hand-rolled Rust PMTiles reader now** — more work for no immediate benefit;
  `go-pmtiles` is correct and pinned.
- **Re-render tiles for the area** — expensive and pointless when the clustered
  source supports a range-read clip.
