# CLAUDE.md

Guidance for working in this repo. Keep it lean — follow the links rather than
inlining their content here.

`iter-maps/server` is a Rust **coordinator/BFF** fronting external engines (OTP
routing, Photon geocoding) plus a build-tier pipeline and a worker tier. The
current-state design is [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Decisions — use the ADR log

**Any architecturally-significant decision requires an ADR** in
[`docs/adr/`](docs/adr/README.md), written in the same change. Read
[`docs/adr/README.md`](docs/adr/README.md) for what counts and the format. When
in doubt, write one. This is not optional.

## Build, lint, test (gcc is installed system-wide — cargo works natively)

```sh
cargo build --workspace --all-targets
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

## Quality bar (non-negotiable)

CI is strict (see [`docs/adr/0006`](docs/adr/0006-strict-ci-and-testing-bar.md)):
fmt, clippy `-D warnings`, build-all-targets, test, `cargo doc -D warnings`,
`cargo deny`, typos, REUSE lint, hadolint. **Run fmt + clippy + test locally and
make them green before committing.** Every component carries tests; new behavior
ships with its tests in the same change.

## Conventions

- **Conventional Commits**; one logical change per commit. Don't push or open PRs
  unless asked — commits stay local until the repo is told it's ready.
- Write code that reads like the surrounding code: match its style and comment
  density. No AI-style over-explaining.
- `concept/` (design blueprint) and `.build-map/` are **local-only reference,
  git-excluded via `.git/info/exclude` — never commit them**. Cite the blueprint
  as "concept doc NN" / "ADR NNNN (concept)", not as repo paths.
- Deferred work is tracked in [`docs/roadmap/`](docs/roadmap/README.md).
