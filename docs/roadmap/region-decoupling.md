# Region decoupling (ADR 0017)

Making the core actually live up to "region-generic" by moving the Italy/Rome
specifics out of generic-named modules — into `regions::<country>[::<city>]`
driver modules where they need real code, and into `region.toml` data where
they're just parameters.

`S`/`M`/`L` = rough effort.

## Done — all region-specific *code* is isolated into drivers

The generic core (steps, jobs, handlers) no longer contains Italy/Rome *logic*;
it dispatches through a trait to the driver for the resolved region's country.
Verified: the only region tokens left in generic code are test fixtures and two
hardcoded default URLs (listed below).

- **Address normalizer** → `regions::italy::address` (`AddressNormalizer` trait),
  selected by region country. `bab96fd`
- **ViaggiaTreno live-trains** → `regions::italy::live_trains`
  (`LiveTrainsProvider` trait); the axum handlers + TTL single-flight stay generic
  in `live_trains.rs`. Unknown country → an inert stub. `9268be9`
- **Rome/ATAC overlay** → `regions::italy::rome` (`TransitOverlayDriver` trait);
  the geometry algorithm stays generic in `steps/overlay.rs` and dispatches
  through it. No driver → overlays skipped. `12883cf`
- **NeTEx-IT id scheme + Trenitalia agency** → `regions::italy::netex`
  (`NetexProfile` trait); the quick-xml parser + `write_gtfs_zip` stay reusable.
  `11d105b`

Adding another country's equivalent now means writing a `regions::<country>`
driver and registering it in the selector — no edits to the generic core.

## Remaining — the config-drive pass (move data into `region.toml`)

ADR 0017 tier 1: the algorithms are generic and the specifics are isolated, but
several specifics are still *constants in code* (now inside the drivers) or
*hardcoded defaults*. Pushing them into `region.toml` means adding a *similar*
region (another Italian city, another GTFS network) needs no recompile at all.
Most of this needs new `iter-region` schema fields, so it's grouped here.

### Two hardcoded default URLs still in generic worker code (the last real leaks)

These remain because the worker doesn't yet resolve its region (see the keystone
below); until it does, they sit as env-overridable defaults in generic code:

- `jobs/rt_reliability.rs` — the ATAC `romamobilita.it` trip-updates URL. `M`
- `main.rs` — the CCISS NAP NeTEx URL (Asset 663391). `S`

### Worker jobs from feeds (keystone — unlocks the two above)

- `main.rs` static job vec → derive the job set from the resolved region's feeds:
  one NeTEx→GTFS job per `source="netex"` feed, one RT job per feed with a
  `realtime=["trip-updates"]` channel. Then the URLs above come from the feed
  entries, and a non-Italy deployment gets the right jobs automatically. `L`

### Driver constants that could become `region.toml` data

The drivers hold these as constants today (isolated, but not yet no-recompile):

- overlay (`regions::italy::rome`): operator, metro line set, colours, the
  branch-split rule, the projection origin → `[[overlays]]` data. `M`
- netex (`regions::italy::netex`): the agency block + the profile id → feed /
  region fields (add a region `timezone`). `M`
- live-trains: base URL + region code are already env-overridable; the defaults
  live in the driver. `S`

### Small generic defaults

- `steps/civici.rs` `country_code 'it'` → region `geocoding.country_codes`. `S`
- `steps/osm.rs` default URL (`italy/centro`) → `region.toml [osm] source_url`. `S`
- `context.rs` / gateway `ITER_REGION` default `italy/lazio/rome` → deploy `.env`.
  `S`
- gateway `enrich.rs` `lang = "it"` default → first token of region
  `geocoding.languages`. `S`

## Verification still owed

A real-clip overlay GeoJSON re-render to confirm byte-identical output (the proof
inputs were cleaned). Low risk — the overlay refactor is a pure relocation of
constants + logic behind a trait, and the unit suite locks the Rome specifics
(B1 split, colours, projection, GTFS keys) — but worth running when the pipeline
next builds the Rome clip.

## Keep generic / shared-contract (no change)

Overture schemas (addresses/places), Photon import, osmium clips, the GTFS-RT
protobuf DTO, the gateway proxy/manifest/offline/styles (already region-driven via
`region.id` + `cfg`). The `itermaps:civico` Photon `object_type` is a cross-tier
contract token — keep it stable (candidate to hoist into `iter-contracts`).
