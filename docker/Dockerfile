# syntax=docker/dockerfile:1
#
# One builder, three slim runtime targets (gateway / pipeline / worker). Build a
# single service with e.g. `podman build -f docker/Dockerfile --target gateway .`
# or let compose select the target per service.

FROM rust:1.95-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --workspace

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*
ENV DATA_DIR=/data
# Production / Kubernetes should run non-root with a writable-volume fsGroup
# (handled in iter-maps/deploy). Under rootless podman the default user already
# maps to an unprivileged host uid.
# USER 10001

FROM runtime AS gateway
COPY --from=builder /build/target/release/iter-gateway /usr/local/bin/iter-gateway
EXPOSE 8090
ENTRYPOINT ["iter-gateway"]

FROM runtime AS pipeline
COPY --from=builder /build/target/release/iter-pipeline /usr/local/bin/iter-pipeline
ENTRYPOINT ["iter-pipeline"]

FROM runtime AS worker
COPY --from=builder /build/target/release/iter-worker /usr/local/bin/iter-worker
ENTRYPOINT ["iter-worker"]
