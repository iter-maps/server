# 0010 — Self-hosted Photon geocoding with civici baked into the index

- **Status:** Accepted
- **Date:** 2026-06-27
- **Supersedes:** —
- **Superseded by:** —

## Context

Geocoding must cover all of Italy in it/en with location bias, served keylessly
(ADR 0002, P1). The reference engine is Komoot Photon. Two ecosystem facts force
the shape:

1. **No ready-made Italy index exists.** GraphHopper publishes for Italy only a
   *search dump* (`photon-dump-italy-1.0-latest.jsonl.zst`), not a serve-ready
   `photon-db` tarball; the only Italy-containing prebuilt index is the ~29 GB
   Europe one. So the index must be **built** from the dump. Photon 1.x (Feb 2026
   cutover) dropped Elasticsearch for **embedded OpenSearch**, needs Java 21, and
   imports a JSON dump with **no Postgres** via `photon.jar import -import-file`.
2. **Italian house numbers (civici) are a coverage requirement, not a nicety.**
   Searching "Via Tripoli 20" drops Rome because OSM holds almost no Italian
   civici — the index can't bias to a number it never stored. The fix is purely
   to index the missing georeferenced numbers; Overture's `addresses` theme has
   them (~25.9M IT points), readable keylessly from public S3 with DuckDB.

The region model (ADR 0008) already carries `geocoding` (dump URL, country,
languages) and `civici` (bbox) per node.

## Decision

We will **build and self-host the Photon index in the pipeline**, region-driven,
with **civici baked into that index**:

- **CIVICI** reads Overture `addresses` by the region's `civici.bbox` with DuckDB
  (in the data-prep image, extensions pre-baked) and emits header-less Photon
  "house" docs — `object_type: itermaps:civico`, `importance 0.00005` so that
  **location bias**, not a fake-high static score, picks the right #20 — deduped
  on (street, number, city).
- **PHOTON** fetches the region's dump, **appends the civici docs** to the import
  stream (with the load-bearing trailing-newline fix), wipes any prior index, and
  runs `import` with `-country-codes`/`-languages` from the resolved region and
  `-extra-tags wikidata,wikipedia,wikimedia_commons` so the enrichment layer
  (ADR 0011) can reach images/ids per feature.
- The engine **serves the index read-write** (embedded OpenSearch writes
  lock/translog even when only serving); the gateway already reverse-proxies
  `/api`, `/reverse`, `/status` unchanged (the client greps Photon `properties`).

Civici live **in the search index**; the structured address↔POI join the
correlation feature needs is a *separate* index (its own ADR), not Photon.

## Consequences

- One geocoding surface returns streets, POIs, stations **and** civici with
  correct location-biased ranking — "clone + up" still holds, now Italy-wide.
- The data-prep image gains the Photon jar + DuckDB CLI; the Overture release id
  (`CIVICI_OVERTURE_RELEASE`) must be bumped as releases expire (~60-day window).
- Reimport is **all-or-nothing**: civici regenerate per build and only reach the
  live index on a full reimport (`FORCE_CIVICI` pairs with `FORCE_PHOTON`).
- Per-row Overture address **license is mixed** (CC-BY / CC0); attribution rides
  the data-license docs, and fusing into ODbL data is handled at display time.
- The full-Italy index (~8M docs + civici, ~2–3 GB, ~10–27 min import) exceeds a
  small dev host; `ROUTING_BOUNDS`-style scope (a micro-state dump, or a tight
  `CIVICI_BBOX`) proves the path, with full Italy on the prod host.

## Alternatives considered

- **A separate civici store merged at the gateway** — better per-record license
  granularity and decouples civici from the reimport cycle, but splits geocoding
  ranking across two systems and loses Photon's unified location bias. We keep
  civici in the index for *search*, and build a separate index only for the
  structured *correlation* join where a join key (not fuzzy search) is needed.
- **Use a prebuilt index** — none exists for Italy in a consumable size.
- **Pre-1.0 Photon (Elasticsearch, `-nominatim-import`)** — obsolete; 1.x is
  OpenSearch with the `import` subcommand and needs no Postgres.
