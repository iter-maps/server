# 0012 — Address correlation: build the address→places index ourselves

- **Status:** Accepted
- **Date:** 2026-06-27
- **Supersedes:** —
- **Superseded by:** —

## Context

Searching an address should surface what is *at* it — the restaurant at "Via
Cavour 1" — as related results, not just the bare civico: record correlations,
don't dedup them away. No open geocoder gives this:
Nominatim parents a POI to its *street*, never its house number; Pelias actively
*deduplicates* a venue against its co-located address; Photon denormalizes the
address onto the POI with no street→number index; Overture embeds a self-contained
address on each place with no foreign key to its addresses theme. So the
address→places mapping has to be built.

Joining on (street, number) in Italy is hazardous: street types abbreviate
(`V.le` = `Viale`), accents and apostrophes vary, esponenti attach to the number
(`12/A`, `12 bis`), `snc` means "no number", and in **Firenze/Genova** a separate
**red** civici series runs alongside the black — `Via X 1` and `Via X 1 rosso`
are *different buildings*. libpostal would normalize streets but is a ~1.8 GB
model and doesn't know the red/black rule.

## Decision

We will **build a per-region addressed-POI index** and correlate in the gateway:

- A **PLACES** pipeline step reads Overture `places` by the discovery bbox with
  DuckDB (same keyless S3 pattern as civici) and writes `output/places.jsonl`
  (id, name, category, freeform address, locality, brand QID, lon/lat).
- The gateway **loads it once into an in-memory bucket index** keyed by a
  **normalized address** — `comune | normalized-street | number(+colour)` — and
  by brand QID. A focused **Italian address normalizer** (in-binary, no
  libpostal) expands the street-type DUG, folds accents, parses esponenti, and
  **lifts the red/black colour into the bucket key** so red never collides with
  black; `snc` is a sentinel, not a number.
- `GET /places/related?street=&housenumber=&city=` returns the places sharing the
  bucket as `sameAddress` relations (and, given `brand`, the chain as
  `sameBrand`). It **attaches, never dedups**; an unknown address returns empty,
  never an error.

The index is a regenerable, read-only artifact each stateless replica loads —
the same posture as tiles and the health document.

## Consequences

- The flagship "what's at this civico" works keylessly on data we already pull;
  the normalizer is small, deterministic, and unit-tested against the Italian
  edge cases (abbreviations, esponenti, red/black, snc).
- Correlation quality is bounded by Overture place-address coverage and the
  freeform split (street vs trailing number); ambiguous cases degrade to no
  match rather than a wrong one.
- Per-row Overture license is mixed (CDLA / CC0 / Apache); fusing it with our
  ODbL data can make the combined output ODbL — carried in the data-license docs.
- The `categories` field is deprecated in Overture's Sept 2026 release (→
  `basic_category`/`taxonomy`); the PLACES query must migrate before then.
- libpostal-grade normalization (full international parsing) is not attempted;
  the rule layer is Italy-scoped by design.

## Alternatives considered

- **libpostal** for normalization — a ~1.8 GB model, not thread-safe, and still
  blind to red/black; overkill for an Italy-scoped join.
- **Correlate inside Photon** — Photon is fuzzy search, not a structured join,
  and its maintainer declined to enumerate house numbers; a bucket index is the
  right tool for an exact address join.
- **Dedup like Pelias/Nominatim** — that destroys exactly the venue↔address pair
  the feature exists to surface.
