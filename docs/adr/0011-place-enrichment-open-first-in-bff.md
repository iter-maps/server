# 0011 — Place enrichment: open-first fusion in the BFF

- **Status:** Accepted
- **Date:** 2026-06-27
- **Supersedes:** —
- **Superseded by:** —

## Context

Geocoding answers "where is X" with a flat GeoJSON list (name + coordinate +
type). The app wants the arrival end of a journey to be a **place panel** —
a one-liner, a summary, a photo, opening hours — not a bare pin (concept doc 20).
That is a fusion-and-ranking layer *above* geocoding, not a geocoder change: the
`/api` response stays Photon-shaped (the client greps those `properties`), so
enrichment is a **separate BFF surface**.

The keyless open layer can supply everything except ratings/reviews: **OSM tags**
(facets + `wikidata`/`wikipedia`/`wikimedia_commons` back-links, already in our
PBF and now in the Photon index via `-extra-tags`, ADR 0010), **Wikidata** (CC0
id hub: labels, `P31`, `P625`, `P18` image, `P856`), **Wikipedia** (CC-BY-SA
summary + thumbnail + the QID for free), and **Wikimedia Commons** (freely
proxiable images with per-file license/author). These are multi-call,
cache-heavy, license-sensitive third parties — exactly what the gateway exists to
proxy/normalize/cache/single-flight.

## Decision

We will build place enrichment as a **gateway surface that fuses open sources
into the normalized `Place` DTO** (`iter-contracts::places`), with **per-field
provenance + license**:

- **Entity resolution hubs on the Wikidata QID.** Seed from the request (an OSM
  `wikidata`/`wikipedia` tag, a QID, or a name+lang) → hop to the QID → fan out:
  Wikipedia REST summary (`/{lang}/api/rest_v1/page/summary/{title}`) for
  description/summary/thumbnail/QID; Wikidata `Special:EntityData/Q….json` for
  structured facts + `P18`.
- **Images come from Commons and are proxied through the BFF.** Resolve `P18` /
  `wikimedia_commons` → a Commons thumbnail (`Special:FilePath?width=N`), fetch
  license/author via the `imageinfo` `extmetadata` API, and serve the bytes
  through a gateway image route. `Image.proxied = true` for the open layer; the
  flag exists so a future commercial source whose ToS forbids proxying is
  `false` (client-direct) — the one documented BFF exception (concept 20 §5).
- **Cache by canonical id with TTL + single-flight** (the existing `TtlCache`),
  since place facts change slowly; send a contact-carrying `User-Agent` (Wikimedia
  now enforces it) and back off on `429`.
- **Attribution is mechanical:** every displayed field carries source + license
  in `provenance[]` (Wikipedia `extract` → CC-BY-SA + link back; Wikidata → CC0;
  Commons image → per-file license + author; OSM → ODbL).
- **Open fallback is mandatory** and commercial sources are out of scope here —
  this ADR is the keyless backbone only.

## Consequences

- The app gets a knowledge card (image + summary + facets) for any place with a
  Wikidata/Wikipedia link, keylessly, behind the same cache/single-flight
  discipline as ViaggiaTreno.
- The gateway gains outbound dependencies on Wikimedia endpoints — rate-limited,
  so aggressive id-keyed caching and `429` backoff are required, and the
  `User-Agent` must carry contact info or requests drop to the lowest tier.
- The Wikimedia Core REST API begins gradual deprecation in **July 2026** (toward
  `api.wikimedia.org`); the summary route works today but must be re-checked
  before public launch.
- CC-BY-SA text obliges share-alike/attribution on display — encoded in
  `provenance`, but the client must render it.

## Alternatives considered

- **Enrich the `/api` response in place** — breaks the Photon wire contract the
  client greps; enrichment is additive, on its own surface.
- **OpenTripMap (pre-fused POIs)** — needs a (free) key, so not strictly keyless;
  we fuse OSM + Wikidata + Wikipedia ourselves, whose inputs we already hold.
- **Client fetches Wikipedia/Commons directly** — leaks attribution/caching
  duties to the app and loses the normalization the BFF posture requires.
