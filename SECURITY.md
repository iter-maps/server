# Security Policy

Iter Maps — server is a **public-facing backend**: instances run on the open
internet and serve other people. A quiet, private disclosure channel therefore
matters more than usual. Please help us keep self-hosters safe by reporting
vulnerabilities privately.

## Reporting a vulnerability

**Do not open a public issue, pull request, or discussion for a security report.**

Use GitHub's **private vulnerability reporting** instead:

1. Go to the repository's **Security** tab.
2. Click **Report a vulnerability** (under *Security advisories*).
3. Describe the issue, the affected version/commit, and reproduction steps.

This opens a private advisory visible only to you and the maintainers. If you
cannot use GitHub's private reporting, open a minimal issue asking for a private
contact channel — **without** including any vulnerability details.

Please include, where you can:

- the affected component (e.g. `iter-gateway`) and version/commit,
- a clear description and impact assessment,
- steps to reproduce or a proof of concept,
- any suggested remediation.

## What to expect

This is a small, maintainer-led project, so timelines are best-effort rather than
contractual:

- **Acknowledgement** of your report within about **5 business days**.
- An initial **assessment** (severity, affected versions) once triaged.
- We practise **coordinated disclosure**: we'll work with you on a fix and a
  release, then publish an advisory. Please give us a reasonable window —
  typically up to **90 days** — before any public disclosure, and we'll keep you
  updated on progress.

We credit reporters in the advisory unless you'd prefer to remain anonymous.

## Scope

This policy covers the code in **this repository**. Vulnerabilities in upstream
engines (OpenTripPlanner, Photon) or third-party dependencies should be reported
to their respective projects; if a flaw is exposed specifically by how this
backend integrates them, report it here.

Note that operational hardening (TLS termination, domain, authentication,
firewalling) is the deployer's responsibility — this backend is designed to sit
behind an external proxy and ships none of its own.
