# 0006 — Strict CI and the testing bar

- **Status:** Accepted
- **Date:** 2026-06-27
- **Supersedes:** —
- **Superseded by:** —

## Context

This is a public-facing server that other people self-host; its images run on
their machines. Quality regressions, license drift, and supply-chain issues are
therefore everyone's problem, not just ours. A lax CI on a public repo invites
exactly those. The owner wants a strict gate.

## Decision

We will gate every push and PR on a strict CI and hold a high testing bar:

- **Format:** `cargo fmt --check`.
- **Lint:** `cargo clippy --workspace --all-targets --all-features -D warnings`.
- **Build & test:** all targets build; `cargo test --workspace` passes.
- **Docs:** `cargo doc` with `-D warnings` (no broken intra-doc links).
- **Supply chain:** `cargo deny` (advisories, licenses, bans, sources).
- **Hygiene:** `typos`, REUSE lint, `hadolint` on the Dockerfile.
- **Coverage** is measured and reported.

Every component carries tests; new behavior ships with tests in the same PR.

## Consequences

- Green CI is a hard merge precondition; "warnings" are errors.
- Adding a dependency means satisfying `cargo deny` (license allow-list,
  advisory-free) — a deliberate friction.
- Contributors must run fmt/clippy/test locally; the workflows mirror that.

## Alternatives considered

- **Minimal CI (build + test only)** — misses lint, license, and supply-chain
  regressions that matter most on a public-facing, self-hosted server.
- **Clippy as warnings** — warnings rot; `-D warnings` keeps the tree clean.
