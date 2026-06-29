# 0008 — Region model: nested composable profiles

- **Status:** Accepted
- **Date:** 2026-06-27
- **Supersedes:** —
- **Superseded by:** —

## Context

The backend must scale from Rome to Italy and toward Europe, but region was
encoded as scattered constants (the `BBOX_LAZIO` /
`PMTILES_BOUNDS` / `CIVICI_BBOX` bboxes, a hardcoded `roma.pmtiles`, fixed
overlay names, a fixed feed list, `TRENITALIA_REGION=5`). Adding a region would
mean editing code in many places. Real transit is hierarchical: national rail,
regional bus/rail, urban networks — and operators span levels (Trenitalia runs
both national long-distance *and* Lazio-regional FL). The pipeline is about to be
built; without a region abstraction it would bake "Rome" into every step.

## Decision

We model regions as a **generic recursive tree of declarative profiles** —
`regions/<path>/region.toml`, nesting = the tree — resolved **root → leaf** by
deep-merge into one effective config. A deployment targets a node
(`ITER_REGION=italy/lazio/rome`); the pipeline, gateway, and worker stay
**region-generic** and consume the resolved config.

A data source is assigned to a node by its **service area, not its operator**:
the all-Italy basemap + geocoding and the national ViaggiaTreno boards live at
the `italy` root; COTRAL/COTRAL-FERRO/FL at `lazio`; ATAC + metro overlays at
`rome`. Merge semantics: scalar fields take the value closest to the target
(last-wins); list fields (feeds, overlays) accumulate down the chain.

## Consequences

- Adding a region (Milan, Paris, all of Europe) is **config + data, no
  recompile** — it fits the "clone + up", regenerable-artifact ethos.
- Country-wide data (basemap, geocoding) is defined once at the root and shared,
  generalizing the "nationwide basemap/geocoding, scoped transit" split.
- Artifacts become `<region>.pmtiles` etc. — this renames `roma.pmtiles`, a
  **client wire-contract change** to coordinate (a known debt to track).
- A new `iter-region` crate owns the schema + resolver; resolution semantics
  must be precise and tested.

## Alternatives considered

- **Flat profiles** — duplicates country-wide data across every region.
- **A Rust module/crate per region** — recompile to add a region, duplication,
  rigid; region data is fundamentally *data*, not code.
- **Keep Rome hardcoded** — guarantees a painful refactor once a second region
  is wanted.
