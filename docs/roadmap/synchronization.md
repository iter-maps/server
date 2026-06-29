# Synchronization / E2EE (PLANNED)

Optional, opt-in cross-device sync of personal state (favorites, home/work,
learned reranking weights) as an **end-to-end-encrypted opaque blob store** — the
single scoped exception to the stateless P7 invariant.

- **Plugs into:** gateway / BFF, a new minimal blob-store surface. The server
  stores only ciphertext it cannot read; no user DB, no auth identity, no
  plaintext personal state.
- **Data deps:** none external — client-encrypted blobs keyed by an opaque
  handle. Distinct from "sense-1" artifact-freshness sync, which is the
  health/freshness manifest, not this.
- **Note:** holds opaque ciphertext only; remains regenerable/discardable.

Decision: ADR 0012
