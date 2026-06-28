# 0018 — Region drivers live in one `iter-region-drivers` crate

- **Status:** Accepted
- **Date:** 2026-06-28
- **Supersedes:** 0017 (driver *placement* only)
- **Superseded by:** —

## Context

ADR 0017 isolated region-specific code behind traits into `regions::<country>`
modules — but *inside each tier crate* (the gateway holds Italy's address +
live-trains, the pipeline holds Italy's overlay, the worker holds Italy's NeTEx).
That works, but it has two costs as we look toward Europe:

1. **One country is scattered across three crates.** "Where is the Italy code?"
   has three answers. The recognizability the isolation was *for* is diluted.
2. **The traits are coupled to tier-private types.** `LiveTrainsProvider` returns
   the gateway's `ApiError`; the overlay/NeTEx traits use tier-local value types.
   So the traits can't be shared without dragging tier internals along.

A crate *per country* (`iter-region-italy`, …) was considered, but at one country
it's ceremony with no payoff: N crates + a shared-traits crate + feature-gating,
and a boundary designed against a single example. A single crate, subdivided by
region, gets the recognizability without the ceremony.

## Decision

Consolidate all region drivers into **one crate, `iter-region-drivers`,
subdivided by region** (`src/<country>/…`, e.g. `src/italy/{address, live_trains,
rome, netex}.rs`). The crate **owns the traits and a registry**, and depends only
on `iter-contracts` + external crates (`reqwest`/`serde`/`async-trait`/`anyhow`) —
**never on the tier crates**:

- traits: `AddressNormalizer`, `LiveTrainsProvider`, `TransitOverlayDriver`,
  `NetexProfile`, plus their value types (`LineKind`, `Projection`, `AgencyInfo`)
  and a neutral provider error (so they stop referencing tier types like the
  gateway's `ApiError`);
- registry: `address_normalizer(country)`, `live_trains_provider(country, …)`,
  `overlay_driver(country, city)`, `netex_profile(id)` → an `Arc<dyn …>`.

The tiers resolve the region (`iter-region`) and call the registry with primitives
(country/city/url); the **generic algorithms stay in their tiers** — the
correlation index, the axum live-trains handlers + cache, the overlay geometry,
the NeTEx parser. Adding a region is a new folder + a registry arm; adding a
country touches no generic code and no other region.

This **supersedes 0017's driver *placement*** (drivers move from each tier's
`src/regions/` into the one crate); 0017's config-drivable-vs-driver split and the
ADR-0017 traits themselves are unchanged.

`iter-region` (the `region.toml` schema + resolver) stays a separate, light crate;
there is **no dependency either way** between it and `iter-region-drivers` — one
resolves config, the other holds code, and the tiers bridge them with primitives.

## Consequences

- One recognizable home for every region's code; `crates/iter-region-drivers/src/
  italy/` is *all* of Italy. Reviewers and a future contributor adding a country
  have a single target.
- The traits become **tier-agnostic** — a genuine cleanup. The gateway's
  live-trains handler now maps the provider's neutral error into its `ApiError`.
- A new crate + a one-time mechanical move (the `regions::<country>` modules and
  their tests relocate; the tiers gain an `iter-region-drivers` dependency and
  repoint imports). No behaviour change.
- If full-Europe binary size ever demands excluding countries, **feature-gate the
  region folders** inside the one crate — still no new crates.

## Alternatives considered

- **A crate per country** — N crates + a shared-traits crate + cross-crate
  feature-gating; ceremony with no payoff at one country, and a boundary designed
  against a single example. A subdivided single crate is lighter and can
  feature-gate folders later if needed.
- **Keep 0017's per-tier modules** — scatters a country across three crates and
  leaves the traits coupled to tier types; the thing this ADR fixes.
- **Fold the drivers into `iter-region`** — pulls `reqwest`/async into the
  currently-light config/resolver crate; keep config and code separate.
