# 0001 — Record architecture decisions

- **Status:** Accepted
- **Date:** 2026-06-27
- **Supersedes:** —
- **Superseded by:** —

## Context

This backend is a ground-up rebuild. As it makes its own implementation choices
— crate boundaries, which engine to front, how to scale — that reasoning needs a
home in the repo, or it evaporates into commit messages and chat logs and gets
re-litigated later.

## Decision

We will keep an ADR log under `docs/adr/`, following the process in its README:
one immutable record per architecturally-significant decision, written in the
same PR as the change, mandatory for significant decisions and rejectable in
review when missing.

## Consequences

- New significant work carries a small writing tax (one short doc) — the point.
- The history of *why* survives contributor turnover and the original author's
  memory.
- `docs/ARCHITECTURE.md` stays the current-state view; the ADR log stays the
  history. They must not drift into duplicating each other.

## Alternatives considered

- **No ADRs, rely on commit messages / PR descriptions** — lost to search and
  squash-merges; no canonical place for "why".
- **A single growing DECISIONS.md** — merge conflicts and no per-decision
  status/supersession.
