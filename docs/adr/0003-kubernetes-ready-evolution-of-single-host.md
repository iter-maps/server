# 0003 — Kubernetes-ready evolution of the single-host model

- **Status:** Accepted
- **Date:** 2026-06-27
- **Supersedes:** —
- **Superseded by:** —

## Context

An earlier invariant fixed "single host, container compose, no cloud
control-plane" as the deployment model. The owner wants this rebuild to also
scale horizontally on Kubernetes — replicas and a worker tier — without
abandoning the "clone + up" single-host default. Statelessness, externalized
artifacts, and graceful drain make these compatible rather than conflicting.

## Decision

We will keep single-host `podman compose up` as the default **and** make the
code Kubernetes-ready: services stateless across requests, all state externalized
to regenerable artifacts, SIGTERM graceful shutdown, liveness/readiness probes,
and a separate worker tier for background jobs. Orchestration manifests
themselves stay out of this repo (they belong in `iter-maps/deploy`).

## Consequences

- A deliberate, owner-approved deviation from the single-host-only model —
  recorded here so it isn't mistaken for an accident.
- Every service must hold no per-client state and must tolerate being replicated
  and rescheduled; readiness must gate traffic on artifact presence.
- The stateful engines (OTP/Photon) scale narrowly (HA, not throughput); the
  stateless tier scales wide.

## Alternatives considered

- **Single host only** — rejected by the owner; forecloses HA and elastic
  scaling.
- **Kubernetes-only, drop single-host** — breaks the self-hoster "clone + up"
  promise that is core to the project.
