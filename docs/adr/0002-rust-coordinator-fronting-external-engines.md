# 0002 — Rust coordinator fronting external engines

- **Status:** Accepted
- **Date:** 2026-06-27
- **Supersedes:** —
- **Superseded by:** —

## Context

The backend must serve journey routing and geocoding. Mature, memory-heavy
engines already solve these well: OpenTripPlanner (GTFS+OSM routing) and Komoot
Photon (OSM geocoding), both JVM. Reimplementing either in Rust would be a
multi-year effort with worse results: the engine choice is swappable, not
load-bearing. What *is* load-bearing is the wire contract, the data pipeline, and
the custom surfaces (tiles, styles, overlays, offline, live-trains).

## Decision

We will build the Rust side as a **coordinator**: a stateless gateway/BFF that
serves the surfaces we own and reverse-proxies routing to OTP and geocoding to
Photon, plus a build-tier pipeline and a worker tier. We orchestrate the engines;
we do not reimplement them.

## Consequences

- The deployment is polyglot (Rust + two JVM engines) — accepted; they are
  isolated containers reached by service name.
- The Rust gateway owns the contract surface and is where future BFF logic
  (itinerary re-ranking, place enrichment) lands.
- Engine upgrades/replacements are contained behind the proxy boundary.
- We depend on the engines' operational characteristics (heap sizing, graph
  build), which the resource/scaling model must account for.

## Alternatives considered

- **Reimplement routing/geocoding in Rust** — enormous scope, worse quality, no
  upside given the contract is engine-agnostic.
- **Expose OTP/Photon directly to clients** — leaks engine specifics into the
  contract and forecloses the BFF layer.
