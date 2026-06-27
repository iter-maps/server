# Contributing to Iter Maps — server

Thanks for your interest. This is a self-hosted, open-source, zero-commercial-key
public-transport backend. Contributions are welcome under the terms below.

## Ground rules (the ethos)

- **No commercial API keys.** Nothing in the stack may depend on a paid/proprietary
  map, geocoding, or routing key (Mapbox, Google, Geoapify, …). Open data and
  self-hostable engines only.
- **Privacy-first.** No readable user state, no product/usage analytics by default.
  Don't add telemetry that phones home.
- **Public-first.** Default to open: code under AGPL-3.0-or-later, docs under CC-BY-4.0.

## Building and testing

The repo is a **Rust workspace** (members under `crates/`). Use a recent stable
toolchain — the pinned channel and MSRV live in `rust-toolchain.toml` and
`Cargo.toml` (`rust-version`).

```sh
cargo build               # build the whole workspace
cargo test                # run all tests
cargo run -p iter-gateway # run the edge service (defaults to :8090)
```

Before opening a PR, please run:

```sh
cargo fmt --all
cargo clippy --all-targets --all-features
cargo test
```

Keep changes surgical and scoped to one logical concern per PR.

## Commit convention

This project uses [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <short description>
```

Types: `feat`, `fix`, `refactor`, `chore`, `docs`, `test`, `style`, `perf`.
Keep the subject under 72 chars; add a body when the change warrants explanation.
The changelog is generated from these messages, so write them accurately.

Never commit secrets, tokens, credentials, or non-public data.

## Architecture decisions

Any architecturally-significant change (a new crate/service, a wire-contract or
config change, a notable dependency, the build/deploy model, the security or
licensing posture) must include an **ADR** in [`docs/adr/`](docs/adr/README.md),
in the same PR. See that README for what counts and the template. A reviewer may
ask for one if a significant change arrives without it.

## CI is strict

Pushes and PRs are gated on fmt, `clippy -D warnings`, build, test, docs,
`cargo deny`, typos, REUSE lint, and hadolint. Run fmt/clippy/test locally first.
New behavior ships with tests in the same change.

## Sign-off: DCO, not a CLA

Contributions are accepted under the **Developer Certificate of Origin (DCO)** —
there is **no CLA**.

The DCO is a lightweight, per-commit attestation: by adding a `Signed-off-by:`
line you certify that you wrote the contribution (or otherwise have the right to
submit it) and that it may be distributed under the project's licenses. The full
text is at <https://developercertificate.org/>.

Sign off every commit:

```sh
git commit -s -m "feat(gateway): add reverse-geocode passthrough"
```

This appends a line using your `git` `user.name` and `user.email`:

```
Signed-off-by: Jane Doe <jane@example.com>
```

Forgot to sign off? Fix the last commit with `git commit --amend -s`, or a range
with `git rebase --signoff <base>`.

**Inbound = outbound.** Your contribution is licensed under the same terms the
project ships: AGPL-3.0-or-later for code, CC-BY-4.0 for docs. We chose the DCO
over a CLA deliberately — it keeps friction low and keeps contributions under the
same copyleft, rather than handing the project broad relicensing rights.

## Licensing of files

This is a multi-license repo by layer: code is **AGPL-3.0-or-later**, docs
(`docs/`) are **CC-BY-4.0**. New files should carry an SPDX header matching
their layer, e.g.:

```
# SPDX-FileCopyrightText: 2026 Iter Maps contributors
# SPDX-License-Identifier: AGPL-3.0-or-later
```

If your change redistributes or serves third-party data, honor that source's
attribution and share-alike obligations (see `DATA_LICENSES.md`).

## Reporting bugs and security issues

Open a regular issue for ordinary bugs and feature requests. For security
vulnerabilities, do **not** open a public issue — follow `SECURITY.md`.
