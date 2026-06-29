# 0015 — GTFS-RT ingestion with a vendored prost subset

- **Status:** Accepted
- **Date:** 2026-06-27
- **Supersedes:** —
- **Superseded by:** —

## Context

Historical reliability needs live GTFS-Realtime delays ingested continuously —
history can't be back-filled, so the recorder must start
logging now even though the value accrues over weeks. ATAC publishes three
keyless GTFS-RT protobuf feeds (trip-updates, vehicle-positions, service-alerts).
Decoding protobuf in Rust normally means `prost` + `prost-build`, but
`prost-build` compiles the `.proto` with **`protoc`** at build time — a system
package this host/CI policy avoids. The full reliability pipeline (the
stable-tuple recorder + the Tier-0/1/2 rollup archives) is a larger build that
also introduces **server-held state** — the scoped exception to the
P7-stateless invariant — which warrants its own decision.

## Decision

We will ingest GTFS-RT in two slices, and this ADR covers the **first**:

- **Decode without `protoc`.** Hand-vendor the minimal GTFS-Realtime message
  subset we read (`FeedMessage` → `TripUpdate` → `StopTimeUpdate`) as
  `#[derive(prost::Message)]` structs with the spec's field tags, depending only
  on the runtime `prost` crate. prost skips unknown fields on decode, so the
  subset is forward-compatible with the full feed.
- **An ingest-only worker job** (`rt-reliability`, 30 s) polls trip-updates,
  decodes it, and derives one delay event per stop keyed on the **stable tuple**
  (route, direction, stop, service-date) — never the raw `trip_id`, which Rome
  renumbers near-daily — dropping incoherent rows (|delay| > 2 h). For now it
  **summarizes**; it does not persist.

The **persistent rollup tier** (Tier-0/1/2 archives, percentile sketches, and the
P7-stateless exception) is deferred to a follow-on ADR when it lands.

## Consequences

- The worker ingests live Rome delays today (proven: 721 trip updates → 13,909
  validated stop events from the real ATAC feed) with no `protoc` in the build.
- The vendored proto must be extended by hand if we later read fields it omits
  (vehicle positions, alerts); the tags are stable GTFS-RT spec values.
- Ingest-only means no reliability *product* yet — the machinery is proven, but
  scores/percentiles wait on the rollup tier (and its state-exception ADR).
- The stable-tuple key and the |delay| validation are the load-bearing
  correctness bits (one bad row poisons a percentile); both are unit-tested.

## Alternatives considered

- **`prost-build` + `protoc`** — pulls a system package the build avoids; the
  hand-vendored subset is small and stable.
- **A third-party `gtfs-rt` crate** — most wrap `prost-build` (so still need
  `protoc`) or vendor generated code we'd not control; a focused in-tree subset
  is simpler and dependency-light.
- **Persist on day one** — couples the provable ingestion slice to the larger
  state-bearing rollup design and its P7 exception; ship ingestion first.
