# Italy/Europe rail + data catalog (PLANNED)

Lift the backend from Rome/Lazio-only to all-Italy and Europe-ready. The scope
splits: **geometry is cheap** (OSM route relations, easily continent-wide),
**routing is expensive** (timetable-graph RAM scales with volume, stays regional,
region-by-region).

- **Plugs into:** the pipeline acquisition layer. The catalog is a
  per-region feed registry — `id, url, format (gtfs|netex), convert, license,
  enabled, optional, insecure` — and the geometry bridge adds an `OVERLAY_BOUNDS`
  clip knob (default `BBOX_LAZIO`) so geometry extent decouples from routing.
- **Data deps:** per-region GTFS direct (e.g. Trenord/Lombardy) and NAP NeTEx
  (Piedmont, Liguria, …) — the latter generalizes the FL NeTEx→GTFS converter
  into a parameterized library; NAP auto-download remains unsolved.
- **Build order:** Phase 0 generalizes the catalog schema (small Rust change);
  routing rollout is many independent per-region sprints.

Decision: ADR 0022, ADR 0021
