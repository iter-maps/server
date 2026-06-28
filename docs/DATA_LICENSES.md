# Data Licenses & Attribution

Everything the Iter Maps backend redistributes or serves carries the upstream's
license obligations. These are **legal duties, not courtesy credits**, and they are
**not the project's to relax** — they are conditions of the right to use the data at
all.

Sources: `concept doc 29` §4 and
`concept doc 03`. Per-source catalog (URLs, refresh, quirks) lives in
`concept doc 03`.

## Redistributed sources

| Source | License | SPDX | Required attribution string |
|---|---|---|---|
| **OpenStreetMap** — basemap, walk/transit network, geocoding index, overlays, water | ODbL 1.0 | `ODbL-1.0` | `© OpenStreetMap contributors` |
| **GTFS — ATAC / Roma Servizi** (static + realtime) | CC-BY 3.0 | `CC-BY-3.0` | `Roma Servizi per la Mobilità S.r.l.` |
| **GTFS — COTRAL** (Lazio extra-urban bus) | **TBD / undeclared** 🔴 | — | — (must be cleared before public production) |
| **GTFS — COTRAL-FERRO** (Lazio regional railways) | **TBD / undeclared** 🔴 | — | — (must be cleared before public production) |
| **Trenitalia FL** (regional trains, NeTEx via CCISS NAP) | **`no_licence_no_contract`** 🔴 | — | open access via the NAP (EU MMTIS regime) but no explicit reuse/redistribution grant — clear with Regione Lazio before public production |
| **House numbers (civici)** — Overture / ANNCSU | CC-BY 4.0 | `CC-BY-4.0` | double: `Overture / OpenAddresses` **and** `ISTAT / Agenzia delle Entrate — ANNCSU` |
| **Glyphs (Noto Sans)** | SIL OFL 1.1 | `OFL-1.1` | bundle license + copyright with the served font files |
| **Komoot Photon** Italy geocoding dump (derives from OSM) | ODbL 1.0 | `ODbL-1.0` | `© OpenStreetMap contributors` |
| **Wikidata** (place enrichment) | CC0 1.0 | `CC0-1.0` | none legally required; credit as courtesy |
| **Overture** (non-address themes) | CDLA-Permissive 2.0 | `CDLA-Permissive-2.0` | include license text when sharing the data; no duty on outputs |
| **Natural Earth / planetiler ancillaries** | public domain (+ ODbL water polygons) | — / `ODbL-1.0` | `Natural Earth` / `OpenMapTiles`; `© OpenStreetMap contributors` for water |

## OpenStreetMap (ODbL) — two output kinds, two duties

ODbL distinguishes two output kinds with **different** obligations:

- **Produced Work** — a *non-database* output: the rendered map, PMTiles basemap,
  MapLibre styles. Duty: **attribution only** — `© OpenStreetMap contributors`
  visible on the map and in tile/style metadata.
- **Derivative Database** — a *database* output: the clipped OSM extract, the Photon
  index, GeoJSON overlays if published as a dataset. Duty: **share-alike** — if
  publicly distributed, it must stay **ODbL**, offering either the derived database
  or the producing code. The project satisfies this naturally: the producing code is
  the public `iter-maps/server` pipeline. Label any *published* derived OSM database
  `ODbL-1.0` and point to the producing code.

## SIL OFL (Noto Sans)

Served unmodified, so the OFL reserved-name and no-standalone-sale clauses are N/A.
Duty: **bundle the license text + copyright** with the served font files.

## TBD-licensed feeds — launch gate 🔴

**COTRAL, COTRAL-FERRO, and Trenitalia FL are undeclared / TBD.** They are usable for
dev/beta but **must be cleared, or shipped disabled by default, before any public
production release.** No TBD-licensed feed ships **enabled by default** until its
license is cleared. (`concept doc 29` §4.3, §6.5; `concept doc 03` "Licenses to clear before
public production".)

## Where attribution must appear (three runtime places)

The same credits must appear in **three** places simultaneously:

1. **In-app** — a visible map credit (OSM + the active transit feeds), reachable from
   an "about / data" screen. This is an obligation, not a nicety.
2. **Tile / style metadata** — the MapLibre style `attribution` field and the PMTiles
   header carry `© OpenStreetMap contributors`.
3. **Repo** — this `DATA_LICENSES.md` / `NOTICE` with the full list, including CC0 /
   CDLA courtesy credits.

This satisfies ODbL Produced-Work attribution, CC-BY for GTFS/civici, and OFL
bundling at once.
