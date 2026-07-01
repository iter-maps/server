# Telemetry & Privacy Posture

Iter Maps is privacy-first by construction (design principle **P7** — no readable
user state). "Telemetry" is **three different concerns** with **three different
rules**. They are never conflated. Conflating them is how privacy-first projects
accidentally betray users.

**Cardinal rule:** no silent phone-home. A self-hosted instance **NEVER** reports
anything to the Iter Maps project or to any central endpoint. There is no central
collector, by design.

Source of policy: ADR 0024.

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
user-data exceptions, because it observes people.

**Status: PLANNED / absent today.** Not implemented in the current backend.

### (b) Operator observability — stays home, never phones home

Instance health, feed freshness, error/latency, and build status, for the operator
running their own machine.

- **On, but LOCAL** to the operator's own host.
- **NEVER** phones home. There is **no central endpoint**.
- This is **not "telemetry" in the phone-home sense at all** — it is an operator
  monitoring their own infrastructure, and is the operator's to keep or discard.
- It is **not user data** (machine/feed health).

Reference model: health.json, smoke test, and logs; Prometheus / Grafana / Loki
on the self-hoster's own host.

**Log schema & correlation:** the structured-log field/label convention
(`service`/`event`/`outcome`/`error.code`/`upstream` labels vs `latency_ms`/
`route`/`status`/`request_id`/… context fields), the `ITER_LOG_FORMAT=json`
switch, and request correlation via `x-request-id` (minted at the gateway edge,
accepting a W3C `traceparent`, propagated to the engines) are defined in
**ADR 0037** and documented on `iter_core::telemetry`.

#### Metrics — internal Prometheus `/metrics` endpoint

Metrics land as **ADR 0037 phase 2**, under the same operator-local,
never-phone-home rule. The gateway installs a process-wide Prometheus recorder at
startup (via `iter_core::telemetry::init` → `iter_core::metrics`) and exposes a
`GET /metrics` endpoint rendering the Prometheus text exposition (version 0.0.4).

- **Internal only.** `/metrics` carries no user data, but it is
  operator-monitoring surface, **not** for public exposure. It shares the exact
  posture of the `/livez` / `/readyz` probes: the external proxy (P3) gates it and
  must **not** route it publicly. A self-hoster who wants it off entirely can set
  `METRICS_ENABLED=0` (it then returns `404`); the default is on.
- **Recording is fail-soft.** It never changes, slows, or breaks a request/
  response, and is a no-op when no recorder is installed.
- **Low-cardinality labels ONLY.** Labels are a bounded set — never the raw
  request path, query string, coordinates, or any user value.
- **Self-scrapes count.** `/metrics` is wrapped by the request middleware, so each
  scrape increments `http_requests_total{method="GET",status="200"}`. This is
  standard Prometheus self-scrape behavior (bounded labels, no cardinality or
  privacy concern) — subtract the scrape rate if you need pure client traffic.

Metric catalog (names/labels/help are defined once in `iter_core::metrics`):

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `http_requests_total` | counter | `method`, `status` | one increment per served request; `method` is normalized to the known HTTP verbs (else `OTHER`), `status` is the numeric HTTP code |
| `http_request_duration_seconds` | histogram | `method` | per-request wall latency (seconds), the same value the request-outcome log line reports |
| `upstream_errors_total` | counter | `upstream`, `code` | a reverse-proxy upstream failure; `upstream=otp\|photon\|viaggiatreno`, `code` = the stable `ApiError` code (e.g. `UPSTREAM_UNAVAILABLE`, `TIMEOUT`) |
| `weather_cache_lookups_total` | counter | `outcome` (`hit\|miss`) | weather-forecast cache lookups on the opt-in rerank path — `hit` served from cache, `miss` fetched upstream |

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
