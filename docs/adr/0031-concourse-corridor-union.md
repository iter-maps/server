# 0031 — Concourse corridor union (footprint dissolve)

- **Status:** Accepted
- **Date:** 2026-06-29
- **Supersedes:** —
- **Superseded by:** —

## Context

ADR 0014 chose pure-Rust overlay geometry and named two `metro-stations`
quality steps as follow-on work: morphological smoothing of the concourse hull,
and a corridor union dissolving a station's overlapping pieces into one
footprint. Smoothing shipped (Chaikin corner-cutting + Visvalingam-Whyatt). The
union was left deferred because it was first scoped against a polygon buffer the
`geo` crate doesn't robustly provide.

`build_metro_stations` synthesizes each station from a concave-hull concourse
plus one side-platform strip per direction-stop, offset perpendicular to the
real track. The hull is built from the platform-ring *points*, so it usually
encloses them — but a strip can poke past the hull edge, leaving overlapping
polygons where the concourse and a strip overhang disagree on the boundary.

The pinned `geo` 0.31 gained `BooleanOps` / `unary_union` (i_overlay-backed,
robust on float coords) after ADR 0014 was written. This removes the original
blocker for the union half without a new dependency. The buffer-based
morphological *close* (rounding concave inlets) still has no robust pure-Rust
primitive and stays deferred.

Invariants that must hold: this is display-only (no routing impact, no GeoJSON
contract change); a station with a single polygon or non-overlapping pieces must
be byte-unchanged (the shipped Rome output is byte-stable); the footprint stays
a valid, closed, simple polygon still covering the stop points; and malformed or
degenerate input must fail soft, never panic.

## Decision

We will **dissolve each station's concourse hull with its overlapping platform
strips into one footprint using `geo::unary_union`**, in local-planar metres,
before projecting to WGS84:

- `station_hull_m` returns the smoothed concourse hull as a metre ring;
  `dissolve_footprint` unions it with that station's platform strips and the
  build path projects the result. The largest output polygon is the footprint.
- **The union is conditional.** Only strips with a vertex *outside* the hull are
  fed to the union; if none escape (the common case — strips already enclosed),
  the raw hull ring is returned **byte-for-byte**, so it never re-traces through
  the boolean op. This is what keeps a single-polygon / non-overlapping station,
  and Rome's shipped output, byte-stable.
- The dissolved ring is guarded like smoothing is: it must be valid, closed, and
  still cover every stop, else we fall back to the raw hull. Non-finite or
  sub-quad strip rings are skipped.

We chose `unary_union` over re-hulling the combined point set because the union
is exact (it follows the true merged boundary rather than re-approximating it
with a concavity parameter) and adds no dependency.

## Consequences

- A station whose platform strip overhangs its concourse now emits one clean
  footprint instead of overlapping pieces; platforms remain as their own
  additive features (the client still draws them as layers).
- No new dependency: `unary_union` is in the already-pinned `geo` 0.31.
- The conditional-dissolve rule is load-bearing for byte stability — any future
  change that always routes the hull through `unary_union` would re-trace
  coordinates and break the Rome golden. The skip-when-contained path is tested.
- The **morphological buffer-close remains deferred** (ADR 0014): `geo` still
  lacks a robust polygon buffer, so concave inlets are not rounded.

## Alternatives considered

- **Re-hull the combined point set** (the dependency-free fallback) — works, but
  re-approximates the merged boundary through the concavity parameter rather
  than following it exactly, and would re-trace every station's ring (breaking
  byte stability); chosen only if `unary_union` were unavailable.
- **Always union (unconditional)** — simpler, but re-traces non-overlapping
  stations through the boolean op, breaking the byte-stable invariant for the
  shipped output; rejected.
- **Buffer-and-close via a polygon buffer** — the originally-scoped approach;
  still blocked on a robust pure-Rust buffer, so left deferred.
