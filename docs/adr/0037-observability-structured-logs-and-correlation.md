# 0037 — Observability posture: structured logs, a field schema, and request correlation

- **Status:** Accepted
- **Date:** 2026-06-29
- **Supersedes:** —
- **Superseded by:** —

## Context

The backend already logged to stdout via `iter_core::telemetry` (an
`ITER_LOG_FORMAT=json` switch over a `tracing-subscriber` fmt layer, an
`EnvFilter`, a one-shot service line). But the logs were unstructured for
operations: no consistent field vocabulary, no per-request outcome line, and no
correlation id tying a gateway log to the engine-side (OTP, Photon) log for the
same request. Upstream failures in the reverse proxy were silent — a
connection-refused or timeout produced the error envelope but no log line. An
operator running Loki/Grafana (the reference stack in `docs/TELEMETRY.md`) had
nothing consistent to index or group by.

The posture that constrains this is ADR 0024's category (b) — **operator
observability stays LOCAL and NEVER phones home**. There is no central
collector; logs are the operator's to keep or discard, and they must carry **no
user data**. Any observability we add lives inside the operator's host.

This is also the foundation for a later stats surface, so the design must be
settled now even though it lands in phases.

## Decision

We adopt one **observability posture** for the whole workstream, landing in
phases. Phase 1 (this change) is structured logging + request correlation, with
**zero new dependencies**.

- **Structured JSON logging.** Keep the `ITER_LOG_FORMAT=json` switch and the
  `EnvFilter`. `service` (`iter-gateway` | `iter-pipeline` | `iter-worker`)
  becomes a subscriber-wide default set once in `iter_core::telemetry::init`, so
  **every** line carries it — one choke-point, no per-call-site repetition.

- **A field/label schema, documented in `iter_core::telemetry`.** Low-cardinality
  **labels** (Loki-indexable): `service`, `event` (dotted category:
  `gateway.request`, `proxy.upstream`, `worker.job`, `pipeline.step`), `outcome`
  (`ok|fail|hit|miss`), `error.code` (the stable `ApiError::code`), `upstream`
  (`otp|photon|viaggiatreno`). Higher-cardinality **context fields** (read, not
  indexed): `latency_ms`, `route`, `status`, `request_id`, `job`, `step`, `feed`,
  `count`. New logs follow this vocabulary; the gateway paths touched here move
  from prose category prefixes to an `event=` field.

- **Request correlation at the gateway edge.** A middleware reads an inbound
  `x-request-id` (or the trace-id of a W3C `traceparent`) when present and valid,
  else **mints** a short id — a monotonic counter mixed with the process start
  nanoseconds, rendered as hex, **no new dependency**. It records the id as
  `request_id` on the request's tracing span (so every line during the request
  carries it), echoes it on the response `x-request-id` header, and stashes it for
  propagation. **Fail-soft:** a missing/malformed/oversized id is never rejected.

- **Hop propagation.** The reverse proxy sets `x-request-id` on the outbound
  calls to OTP (routing) and Photon (geocoding), so a gateway line correlates to
  the engine-side one.

- **Per-request outcome logging.** A customized `tower-http` `TraceLayer` opens a
  `gateway.request` span (method, route, `request_id`) and logs exactly **one**
  INFO line per request with `status` + `latency_ms` (WARN on failure) — without
  lowering the global filter to debug.

- **Upstream-failure logging.** The previously-silent proxy upstream errors emit
  a WARN with `event="proxy.upstream"`, `upstream`, `error.code`, and the reqwest
  cause. The error envelope (response body/status) is unchanged.

- **Metrics — NEXT phase, not here.** Metrics land later via the Rust `metrics`
  facade behind an **internal** Prometheus `/metrics` endpoint (the foundation for
  a future admin/public stats dashboard). It is kept operator-local and never
  public; a curated public stats summary is a separate, later surface.

## Consequences

- A consistent field/label schema and end-to-end correlation across the
  gateway→engine hop. An operator can group by `service`/`event`/`outcome`, and
  follow one `request_id` from the gateway line to the OTP/Photon line.
- The metrics dependency and `/metrics` endpoint arrive in the next phase; the
  broad worker/pipeline category cleanup (prose → `event=`) and per-step durations
  are phased in later. This change moves only the gateway paths it touches.
- Correlation/logging is fail-soft by construction: it never rejects a request,
  never changes a response body/status, and adds only cheap work — so it can't
  break or meaningfully slow a request.
- Honors ADR 0024 and `docs/TELEMETRY.md`: everything stays operator-local, no
  phone-home, no user data in logs.

## Alternatives considered

- **Metrics as log events (count/aggregate from logs).** Rejected as the metrics
  path: a Prometheus `/metrics` endpoint is the right foundation for the planned
  stats dashboard and for Grafana. Logs + Loki remain for log analytics; the two
  are complementary, not a substitute.
- **Lower the global filter to debug to get request logs.** Rejected — spammy and
  it sprays default framework spans. A customized `TraceLayer` gives exactly one
  INFO line per request at the normal filter level.
- **Add a UUID/RNG dependency for the request id.** Rejected for this phase — a
  monotonic+entropy hex id is enough to correlate within one operator's log
  stream, and it keeps the change dependency-free (and `cargo deny`-clean).
- **Record `service` per call site.** Rejected — error-prone and noisy; a
  subscriber-wide default is one choke-point.
