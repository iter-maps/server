# Crowd telemetry (PLANNED)

Opt-in, anonymized device telemetry used as a **low-weight, never-primary**
signal: tune crowding/reliability/traffic factors and gap-fill no-RT lines
(metro, COTRAL, FL). Second scoped exception to stateless P7.

- **Plugs into:** a worker ingest + fusion pipeline (HMM map-matching, trust /
  anti-abuse, Bayesian fusion with a hard weight cap + time decay) emitting a
  synthetic GTFS-RT layer for no-RT lines + aggregates into the reliability
  archive; consumed by the gateway reranker.
- **Data deps:** consenting client telemetry only. Privacy gates: rotating
  tokens, k-anonymity, geo-indistinguishability, ephemeral ingest window — no
  readable per-user state.
- **Build order:** wave 1 logs aggregates only; later waves add metro position
  inference, synthetic RT, road FCD.

Design: concept doc 22 — crowd-telemetry ·
Decision: ADR 0016
