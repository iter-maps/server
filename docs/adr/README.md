# Architecture Decision Records

This log records the **architecturally-significant decisions** made building
`iter-maps/server`. Each ADR is a small, immutable document capturing *one*
decision: its context, what was decided, and the consequences.

> These are **this repository's implementation decisions**. They are distinct
> from the design blueprint's rationale capsules (referenced elsewhere as
> "ADR NNNN (concept)"), which justify the *design* a rebuild honors. When a
> repo ADR realizes a blueprint decision, it cites it.

## When an ADR is mandatory

Write one — in the **same PR** as the change — for any decision that:

- adds, removes, or restructures a crate, service, or cross-crate boundary;
- changes a public wire contract, on-disk layout, or config surface;
- adds or drops a non-trivial dependency, or picks one tool/engine over another;
- changes the build, container, deployment, or scaling model;
- affects the security, privacy, or licensing posture;
- you would otherwise have to explain at length in a PR description.

**If you are unsure, write one.** A reviewer may reject a significant change that
lacks its ADR. Trivial changes (a bug fix, a refactor with no interface change,
a typo) do not need one.

## How

1. Copy [`0000-template.md`](0000-template.md) to `NNNN-kebab-title.md`, where
   `NNNN` is the next zero-padded integer.
2. Fill every section. Keep it short — one decision, the *why*, the trade-offs.
3. Open it `Status: Proposed`; it becomes `Accepted` on merge.

## Rules

- **One decision per record.**
- **Immutable once Accepted.** Don't rewrite an accepted ADR to reflect a new
  decision — write a new one and mark the old `Superseded by NNNN`. The log is a
  history, not current-state docs (that's [`../ARCHITECTURE.md`](../ARCHITECTURE.md)).
- **Numbered, never reused.** Gaps are fine; reuse is not.

## Status lifecycle

`Proposed` → `Accepted` → (`Superseded by NNNN` | `Deprecated`)

## Index

- [0001](0001-record-architecture-decisions.md) — Record architecture decisions
- [0002](0002-rust-coordinator-fronting-external-engines.md) — Rust coordinator fronting external engines
- [0003](0003-kubernetes-ready-evolution-of-single-host.md) — Kubernetes-ready evolution of the single-host model
- [0004](0004-single-gateway-binary.md) — One gateway binary for all Rust-owned surfaces
- [0005](0005-layered-licensing-reuse-dco.md) — Layered licensing, REUSE, and DCO
- [0006](0006-strict-ci-and-testing-bar.md) — Strict CI and the testing bar
- [0007](0007-offline-extraction-via-go-pmtiles.md) — Offline extraction via the go-pmtiles CLI
- [0008](0008-region-model-nested-profiles.md) — Region model: nested composable profiles
- [0009](0009-otp-graph-built-in-pipeline.md) — OTP routing graph built in the pipeline
- [0010](0010-self-hosted-photon-with-civici-in-index.md) — Self-hosted Photon geocoding with civici baked into the index
- [0011](0011-place-enrichment-open-first-in-bff.md) — Place enrichment: open-first fusion in the BFF
- [0012](0012-address-correlation-build-it-yourself.md) — Address correlation: build the address→places index ourselves
- [0013](0013-gateway-resolves-region.md) — The gateway resolves the region at startup
- [0014](0014-pure-rust-overlay-geometry.md) — Pure-Rust overlay geometry (no Python/shapely)
- [0015](0015-gtfs-rt-ingestion-vendored-prost.md) — GTFS-RT ingestion with a vendored prost subset
- [0016](0016-fl-netex-to-gtfs-converter.md) — FL NeTEx→GTFS converter in the worker
