# 0021 — FL calendar from ValidDayBits (calendar_dates-only)

- **Status:** Accepted
- **Date:** 2026-06-29
- **Supersedes:** 0016 (calendar emission only)
- **Superseded by:** —

## Context

ADR 0016 built the NeTEx→GTFS converter and derived the GTFS calendar from each
`DayType`'s `DaysOfWeek` over the journeys' `ValidBetween` window — and flagged
that it did not expand the NeTEx `UicOperatingPeriod` bit-patterns, so the
calendar was faithful only for a single-week window. The real FL feed encodes
exact service days: a `UicOperatingPeriod` carries `FromDate`/`ToDate` and a
`ValidDayBits` string with one bit per day in `[FromDate..ToDate]` (`1` = runs),
linked to a `DayType` through a `DayTypeAssignment`. Over more than a week,
`DaysOfWeek`-over-a-window cannot express a weekday holiday exclusion;
`ValidDayBits` can. (Verified against the real feed: 1594 operating periods,
`bit[i]` ↔ `FromDate + i days`, in clean 1:1 correspondence with DayTypes and
journeys.)

## Decision

Derive the FL calendar from `ValidDayBits`: resolve each service (its `DayType`)
through the `DayTypeAssignment` to a `UicOperatingPeriod`, expand `ValidDayBits`
to the exact running dates, and emit **`calendar_dates.txt` only** — every row
`exception_type=1` (added service); **`calendar.txt` is no longer emitted**. A
service whose bits are all `0` has no dates and is dropped along with the trips
that reference it, keeping the GTFS referentially complete. `calendar_dates`-only
is valid GTFS and accepted by OTP.

Date arithmetic for the bit expansion is a small self-contained
proleptic-Gregorian day-count (no new dependency).

## Consequences

- The FL calendar is exact for any span: weekday holiday exclusions and irregular
  runs are encoded precisely, not approximated by a weekly mask.
- The emitted GTFS has no `calendar.txt`; consumers must accept
  `calendar_dates`-only — OTP does. A fresh FL→OTP graph load on the new feed
  confirms end-to-end (the converter output is valid GTFS by construction).
- The `DaysOfWeek`/`ValidBetween` parse is retained only to bound the feed's date
  span; it no longer drives the emitted calendar.

## Alternatives considered

- **Keep `DaysOfWeek` over `ValidBetween` (ADR 0016)** — faithful only for a
  single week; the limitation this fixes.
- **`calendar.txt` for the span + `calendar_dates` for the holes** — reintroduces
  a weekly mask just to subtract from it, more code, and degenerates to one
  exception row per off-day for irregular runs. `calendar_dates`-only encodes the
  flat date list the data already is.
