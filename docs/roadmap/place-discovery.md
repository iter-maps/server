# Place discovery & enrichment (PLANNED)

Assemble rich place knowledge (category taxonomy, description, photo, hours,
ratings) server-side from many sources into one neutral DTO with per-field
provenance — consumes geocoding for the "what did the user tap" anchor.

- **Plugs into:** gateway / BFF fusion engine — entity resolution (QID → OSM-id →
  proximity+name dedup), field-level precedence merge, ranking blend (distance +
  open-now + personal weights).
- **Data deps:** keyless open layer first — OSM, Wikidata, Overture, Wikipedia,
  Wikivoyage, OpenTripMap. Commercial opt-ins (Foursquare, TripAdvisor, Geoapify,
  Yelp, Google) are flagged, cached per-ToS, with graceful open fallback.
- **Build order:** wave 1 open layer; wave 2 Wikivoyage editorial; wave 3
  commercial overlay.

Design: concept doc 20 — place-discovery ·
Decision: ADR 0014
