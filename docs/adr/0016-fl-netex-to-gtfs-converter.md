# 0016 — FL NeTEx→GTFS converter in the worker

- **Status:** Accepted
- **Date:** 2026-06-28
- **Supersedes:** —
- **Superseded by:** —

## Context

The Lazio FL regional-rail lines have no routable public GTFS — Trenitalia
publishes them only as **NeTEx** (the EU/IT profile) via the CCISS NAP. Without a
GTFS feed the OTP graph can't route FL journeys, so something must synthesize one
(the routing region declares `TRENITALIA-FL` as a `netex`-source feed, which the
pipeline's GTFS step deliberately skips, leaving the slot for the worker). The
real dataset (`IT-ITI4-0083`) is a single ~58 MB XML document with ~450 stops,
5 lines, ~1,600 journeys and ~20,600 passing times — too large to hold a DOM in
memory, and deeply nested (journeys reference stops indirectly through a journey
pattern). The NAP is login-gated, so the file is normally *placed*, not fetched.

## Decision

We will convert NeTEx→GTFS **in the worker, in-tree**, with a streaming pull
parser (`quick-xml` over a `flate2` gunzip stream — both pure Rust, no system
deps):

- parse once into an intermediate model: `Line`→route, `ScheduledStopPoint` (with
  its own `Location`)→stop, `ServiceJourneyPattern`→the ordered
  `StopPointInJourneyPattern` sequence, `ServiceJourney`+`passingTimes`→trip +
  stop_times (resolving each passing time's `StopPointInJourneyPatternRef` back to
  its stop), `DayType` days-of-week + the journeys' `ValidBetween`→the calendar;
- emit a referentially-complete GTFS zip (agency/stops/routes/calendar/trips/
  stop_times), dropping only rows that can't resolve, written atomically to
  `<graph>/TRENITALIA-FL.gtfs.zip` where the OTP graph build picks it up;
- the FL job runs on startup + daily and **auto-downloads** the NeTEx from the
  Italian NAP (CCISS) public endpoint on each run (`NETEX_URL`, default the RAP
  Lazio L2 asset) — the daily cadence is the refresh; set `NETEX_URL=` empty to
  use a file placed at `GATEWAY_NETEX_PATH` instead.

## Consequences

- FL routing becomes possible: a faithful GTFS is produced from the real NeTEx
  with no data loss (proven: 450 stops / 5 routes / 1,594 trips / 20,617
  stop_times / 1,594 services, matching the NeTEx counts, in ~4 s).
- Two new worker deps (`quick-xml`, `flate2`); the parser is hand-written against
  the observed IT-profile shape, so a profile change (different nesting, NeTEx
  versions) may need parser updates.
- The calendar uses each `DayType`'s `DaysOfWeek` over the journeys' validity
  window — it does **not** yet expand the `UicOperatingPeriod` bit-patterns into
  `calendar_dates` exceptions; for a single-week window this is faithful, but a
  longer feed with holiday exceptions would need that refinement.
- **NAP auto-download works**: the CCISS NAP serves the NeTEx over an
  unauthenticated HTTP GET (no login), so the job fetches it directly. The
  default pins a stable asset id; if the NAP rotates ids, resolving the asset by
  filename from the NAP catalog JSON is the documented fallback. The data carries
  `no_licence_no_contract` (NAP access regime, redistribution unstated — see
  `DATA_LICENSES.md`).
- Shapes (`shapes.txt`) are not stitched (OTP routes without them); a future
  refinement can derive them from OSM rail.

## Alternatives considered

- **A DOM/serde XML parse** — won't scale to a 58 MB document; streaming is
  required.
- **An external NeTEx→GTFS tool** (e.g. a JVM converter) — adds a runtime to the
  lean worker for one feed; the in-tree streaming parser is dependency-light.
- **Convert in the pipeline tier** — the FL feed refreshes on the worker's daily
  cadence and is the worker's existing responsibility; keep it there.
