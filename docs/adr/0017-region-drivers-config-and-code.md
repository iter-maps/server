# 0017 — Region specifics: config-driven where possible, drivers where code is needed

- **Status:** Accepted
- **Date:** 2026-06-28
- **Supersedes:** —
- **Superseded by:** 0018 (driver *placement* only; the config-vs-driver model and
  the traits stand)

## Context

ADR 0008 made region a **declarative tree** (`regions/<path>/region.toml`,
resolved root→leaf) and committed the pipeline/gateway/worker to staying
**region-generic**, with adding a region being "config + data, no recompile." It
even rejected a Rust module/crate per region.

Two things ADR 0008 didn't fully reckon with have since surfaced as we built the
real capabilities:

1. **The processing code leaks Italy/Rome/ATAC into generic-named modules.** The
   overlay step hardcodes `operator == "ATAC"`, the metro line set `{A,B,C}`, the
   B1/Jonio/Conca branch split, and the contract colours; the FL NeTEx→GTFS
   converter assumes the `IT:ITI4` id structure and an FL/`Europe/Rome`/`it`
   agency; the civici step is Italy-specific; the gateway's address normalizer
   (DUG, esponenti, Florence/Genova red-black) and the ViaggiaTreno live-trains
   proxy are Italy-specific; the ATAC GTFS-RT and CCISS NAP URLs are hardcoded.
   So the code does not actually live up to 0008's "region-generic."
2. **Some feeds need custom *code*, not just config.** A standardised GTFS feed is
   pure config (a URL). A NeTEx feed needs a parser; an operator's overlay needs
   its line/colour/branch rules; a country's addresses need a normalizer; a
   country's live-trains need that API's shape. You cannot config-drive a parser.
   ADR 0008's "no recompile, ever" was too absolute for these.

## Decision

Region specifics are handled in **two tiers, both keyed by the region TOML**, and
**region-specific code never lives in a generic-named module**:

1. **Config-drivable specifics → `region.toml` data (no code, no recompile).**
   The generic algorithm reads region-supplied parameters. This covers the
   *tunable* specifics, extending 0008's data approach:
   - overlays: the operator, the line set with per-line colour + branch-split
     rules (so the overlay generator is generic, Rome's lines are data);
   - feed/source URLs (ATAC GTFS-RT, the CCISS NAP NeTEx asset), agency name,
     timezone;
   - existing data (bboxes, feeds, civici bbox, `live_trains.region_code`).
   Adding a *similar* region (another Italian city, another GTFS network) is
   config-only — 0008's no-recompile promise holds here.

2. **Custom-format processing → a driver module, selected by a TOML id.** When a
   feed/surface genuinely needs custom code, the TOML names a driver by id
   (`feed.source = "netex"`, `live_trains.provider = "viaggiatreno"`,
   `address.profile = "it"`, an overlay `generator`), and the code implementing
   it lives in a **`crates/<crate>/src/regions/<country>/…` module tree that
   mirrors the `regions/<country>/` config tree**, behind a generic trait the
   core dispatches through. Rust compiles crates, not the `regions/*.toml` data
   dirs, so the driver code lives in `src/regions/`, organized by the same path —
   not hardcoded into a generic step/handler.

This narrows ADR 0008 honestly: **standardised feeds stay config (no recompile);
a genuinely new format adds a thin driver module + a TOML id** — not the whole
region as code (the thing 0008 rightly rejected). The generic core is untouched
when adding a region.

## Consequences

- The generic core (steps, jobs, handlers) carries no operator/country constants;
  it dispatches on the resolved config. Reviewers can see at a glance what's
  generic vs. which country a driver serves.
- Adding a region: config-only if its feeds are standardised or fit an existing
  driver; otherwise one new `regions::<country>` driver + a TOML id. No edits to
  generic code, no other region affected.
- A migration cost: the existing Italy/Rome code (overlay ATAC params, NeTEx,
  civici, address normalizer, ViaggiaTreno, hardcoded URLs) must be moved behind
  this structure — done incrementally, generic-first, each step staying green and
  re-proven on the real Rome/Lazio data. Tracked in the roadmap.
- The region schema (`iter-region`) grows the selecting ids + overlay/feed params;
  it stays the single declarative source of truth.

## Alternatives considered

- **Keep 0008 literally (everything is config, no code per region)** — impossible
  for custom formats (a NeTEx parser is code); the current "hardcode it in a
  generic module" is how that gap was filled, and it's the leak we're fixing.
- **A full crate per region** — 0008 rejected it (recompile + duplication for
  *all* region data); a thin driver *module* for only the custom-code parts keeps
  region *data* in TOML and isolates only the genuinely-custom code.
- **Dynamic plugin loading (dlopen/wasm)** — runtime complexity unjustified for a
  statically-built, self-hosted single binary; compiled drivers selected by
  config are simpler and type-safe.
