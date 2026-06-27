# Place discovery & enrichment (WAVE 1 BUILT)

Assemble rich place knowledge (category taxonomy, description, photo, hours,
ratings) server-side from many sources into one neutral DTO with per-field
provenance — consumes geocoding for the "what did the user tap" anchor.

**Wave 1 is built and proven** (ADR 0011 enrichment, ADR 0012 correlation):
`GET /places/enrich` fuses Wikipedia + Wikidata + Wikimedia Commons into the
normalized `Place` DTO with per-field provenance, `/places/image` proxies the
Commons image, and `/places/related` correlates the places sharing a searched
civico (an in-binary Italian address normalizer + Overture-POI bucket index).

- **Plugs into:** gateway / BFF fusion engine — entity resolution (QID → OSM-id →
  proximity+name dedup), field-level precedence merge, ranking blend (distance +
  open-now + personal weights).
- **Data deps:** keyless open layer first — OSM, Wikidata, Overture, Wikipedia,
  Wikivoyage, OpenTripMap. Commercial opt-ins (Foursquare, TripAdvisor, Geoapify,
  Yelp, Google) are flagged, cached per-ToS, with graceful open fallback.
- **Build order:** ✅ wave 1 open layer (enrichment + correlation) · 🚧 wave 2
  Wikivoyage editorial collections · 🚧 wave 3 commercial overlay · 🚧 the fuller
  entity-resolution + ranking blend.

Design: concept doc 20 — place-discovery ·
Decision: ADR 0011 (enrichment), ADR 0012 (correlation)
