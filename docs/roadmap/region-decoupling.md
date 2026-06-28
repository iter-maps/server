# Region decoupling (ADR 0017)

Making the core actually live up to "region-generic" by moving the Italy/Rome
specifics out of generic-named modules — into `region.toml` data where they're
just parameters, or into `regions::<country>` driver modules where they need
real code. Source: the leakage audit of the three core crates (2026-06-28); the
leakage is concentrated, most of it config-drivable.

Each migration is **generic-first, stays green, and is re-proven on the real
Rome/Lazio data**. `S`/`M`/`L` = rough effort.

## Done

- **Italian address normalizer → `regions::italy::address` driver** behind the
  generic `AddressNormalizer` trait, selected by region country
  (`bab96fd`). Establishes the `regions::<country>` structure the rest follows.

## Tier 1 — config-drivable (push to `region.toml`, algorithm stays generic)

### Pipeline · `steps/overlay.rs` (the one heavy leak in the crate)

The metro/transit overlay generator hardcodes ATAC/Rome throughout. Push the
data to `[[overlays]]`; keep the geometry algorithm generic:

- operator filter `"ATAC"` → an `operator` (or `operators`) field. `S`
- metro line set `{A,B,C}` → use the existing-but-ignored `overlays.lines` (or a
  `metro_lines` set); a route is "metro" iff its ref is in that set. `M`
- per-line colours (A `#E27439`, B/B1 `#0570B5`, C `#008456`) → an
  `[overlays.colors]` ref→hex table + a configurable default. `S`
- `ME{line}` / `ATAC:` id conventions → derive the prefix from the overlay's feed
  (`Feed.id`), not literals. `M`
- central-Rome planar origin (`12.5, 41.9, M_LON=82_800`) → derive from the
  region extent centroid (`M_LON = 111_320·cos(lat)`). `S`
- `ATAC.gtfs.zip` filename → `<feed.id>.gtfs.zip`. `S`
- B1 / Jonio–Conca branch split → `[[overlays.branch_split]] line="B"
  branch="B1" termini=["jonio","conca"]` (simple enough for config; no driver
  needed). `M`

### Pipeline · other

- `steps/civici.rs` `country_code 'it'` → region `geocoding.country_codes` (add a
  `Context::country_code()`). `S`
- `steps/osm.rs` default URL (`italy/centro`) → `region.toml [osm] source_url`
  (env override stays). `S`
- `context.rs` `ITER_REGION` default `italy/lazio/rome` → a deploy `.env` default,
  not a code literal. `S`

### Worker · derive jobs from the region's feeds (the keystone)

- `main.rs` static two-job vec → build the job set from the resolved region: one
  NeTEx→GTFS job per `source="netex"` feed, one RT-reliability job per feed with
  a `realtime=["trip-updates"]` channel. Non-Italy deployments then get the right
  jobs automatically. `L`
- NAP NeTEx URL (CCISS Asset 663391) → the FL `[[feeds]]` entry `url`. `S`
- netex/out file paths (`trenitalia-fl…`, `TRENITALIA-FL.gtfs.zip`) → derived
  from `<feed.id>`. `S`
- `jobs/rt_reliability.rs` ATAC trip-updates URL → the ATAC `[[feeds]]` realtime
  URL; one job per RT feed. `M`
- `netex.rs` synthesized agency block (Trenitalia / trenitalia.com / Europe/Rome
  / it / FL) → feed + region fields (add a region `timezone`). `M`

### Gateway · config defaults

- `config.rs` `region_code.unwrap_or(5)` → drop the Lazio `5` fallback; rename
  `trenitalia_region` to a provider-neutral name. `S`
- `config.rs` `viaggiatreno_url` default → the viaggiatreno driver / `[live_trains]
  base_url`. `S`
- `config.rs` `ITER_REGION` default `italy/lazio/rome` → deploy `.env`. `S`
- `enrich.rs` `lang = "it"` default → first token of region `geocoding.languages`.
  `S`

## Tier 2 — driver modules (`regions::<country>`, selected by a TOML id)

- **ViaggiaTreno live-trains** — `trenitalia.rs` is a whole ViaggiaTreno/RFI
  provider (IT JSON field names, endpoint segments, `S\d+` station ids,
  `Europe/Rome` + CET/CEST date param, the viaggiatreno.it referer). Move to
  `regions::italy::live_trains::viaggiatreno` behind a `LiveTrainsProvider` trait,
  selected by `[live_trains] provider = "viaggiatreno"` (the field already exists
  on the profile, unused today). Serve the generic `/live-trains/*` (keep
  `/trenitalia/*` as an alias). `L`
- **NeTEx IT id scheme** — `netex.rs` `gid()` strips the `IT:ITI4:` codespace;
  other countries' NeTEx use a different prefix shape. Move to
  `regions::italy::netex` as a `NetexIdScheme`, selected by a per-feed
  `netex_profile = "it-iti4"`; the generic `parse`/`write_gtfs_zip` stay in core.
  `M`

## Keep generic / shared-contract (no change)

Overture schemas (addresses/places), Photon import, osmium clips, the GTFS-RT
protobuf DTO, the gateway proxy/manifest/offline/styles (already region-driven via
`region.id` + `cfg`). The `itermaps:civico` Photon `object_type` is a cross-tier
contract token shared with the gateway — keep it stable (candidate to hoist into
`iter-contracts`).
