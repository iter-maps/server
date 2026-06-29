# Stations & in-station routing (PLANNED)

Derive a single OSM-indoor station model (platforms, entrances/exits, corridors,
levels, stairs, elevators) that feeds **two consumers** from one pipeline pass:
the station-cutout overlay (display) and the routing engine (accurate egress +
transfers).

- **Plugs into:** the pipeline station-extraction step, extending the Rome metro
  cutout. New output: GTFS Pathways (`pathways.txt`, `levels.txt`) +
  `transfers.txt` consumed by OTP (boarding-location snapping, subway-entrance
  fallback rungs).
- **Data deps:** OSM indoor structures. Gate per station — Pathways are
  all-or-nothing, so emit only when the derived graph is fully connected; else
  fall back to lighter rungs. OSM indoor sparsity (48/74 Rome stations lack full
  geometry) needs conservative synthesis flagging.
- **Build order:** Rome/Lazio first (ATAC/COTRAL); other regions as OSM indoor
  coverage or NAP StopPlace+PathLink improves.

Decision: ADR 0019
