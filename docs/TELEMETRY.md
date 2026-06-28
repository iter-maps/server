# Telemetry & Privacy Posture

Iter Maps is privacy-first by construction (design principle **P7** — no readable
user state). "Telemetry" is **three different concerns** with **three different
rules**. They are never conflated. Conflating them is how privacy-first projects
accidentally betray users.

**Cardinal rule:** no silent phone-home. A self-hosted instance **NEVER** reports
anything to the Iter Maps project or to any central endpoint. There is no central
collector, by design.

Source of policy: the project's OSS-operations design (concept doc 29 §5) and
ADR 0024.

## The three concerns

| Concern | What it is | Default | Phones home? | Rule |
|---|---|---|---|---|
| **(a) Crowd / rider telemetry** | Consenting riders' GPS/sensor traces, used for no-realtime gap-fill and factor tuning | **opt-in, OFF** | only to the user's chosen instance; anonymized/aggregate | scoped P7 exception, strictest privacy bar |
| **(b) Operator observability** | Instance health, feed freshness, error/latency, build status | **on, LOCAL** | **NEVER** | not user data; operator-owned |
| **(c) Product / usage analytics** | "How many users tapped X" | **NONE** | **NEVER** | absent by default |

### (a) Crowd / rider telemetry — opt-in, off by default

A stream of consenting riders' device telemetry that fills no-realtime gaps and
tunes ranking factors.

- **Opt-in, OFF by default.** A user who never enables it creates **zero** server
  state.
- **Anonymized** — rotating ephemeral tokens, no account or device id.
- **Aggregate-not-raw** retention; k-anonymity, optionally
  geo-indistinguishability / differential privacy.
- Reports **only to the user's chosen instance** — never to the project.
- **Self-hoster-disableable.**

It is a **scoped exception** to P7 (no readable user state) — the strictest of the
user-data exceptions, because it observes people. Full model:
`concept doc 22`.

**Status: PLANNED / absent today.** Not implemented in the current backend.

### (b) Operator observability — stays home, never phones home

Instance health, feed freshness, error/latency, and build status, for the operator
running their own machine.

- **On, but LOCAL** to the operator's own host.
- **NEVER** phones home. There is **no central endpoint**.
- This is **not "telemetry" in the phone-home sense at all** — it is an operator
  monitoring their own infrastructure, and is the operator's to keep or discard.
- It is **not user data** (machine/feed health).

Reference model: `concept doc 12` (health.json, smoke test,
logs; Prometheus / Grafana / Loki on the self-hoster's own host).

### (c) Product / usage analytics — none by default

- **NONE by default.** No Firebase Analytics, no Google Analytics.
- Such tools are excluded twice over: (1) they require a commercial platform, which
  breaks **P1** (no commercial API keys); and (2) Google Analytics is, by repeated
  EU regulator rulings, unlawful for an EU app (the Italian Garante banned it on
  9 June 2022).
- **If product analytics is ever added**, it must be **self-hosted FOSS, opt-in, no
  PII, cookieless** (e.g. Plausible or Umami), and **public-instance only**.
- **Self-hosted instances must NEVER silently report to the project** — same rule
  as (b).

**Status: NONE today, PLANNED-only if ever added.** No analytics ships in the
current product.

## Who owns compliance

The project ships each capability **privacy-safe by construction**. **Each
self-hoster is their own data controller** and owns their own compliance (GDPR
lawful basis, privacy policy) for whatever they choose to enable. The app ships a
privacy policy stating that (a) is opt-in and (c) is absent.

A `TELEMETRY.md` lives in both `iter-maps/server` and the app repo so the posture is
stated out loud, in every relevant place.
