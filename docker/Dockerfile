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
# go-pmtiles for the offline range-extract (concept ADR 0010 / repo ADR 0007).
ARG PMTILES_VERSION=1.30.3
RUN curl -fsSL -o /tmp/pmtiles.tgz \
      "https://github.com/protomaps/go-pmtiles/releases/download/v${PMTILES_VERSION}/go-pmtiles_${PMTILES_VERSION}_Linux_x86_64.tar.gz" \
 && tar -xzf /tmp/pmtiles.tgz -C /usr/local/bin pmtiles \
 && rm /tmp/pmtiles.tgz \
 && pmtiles version
COPY --from=builder /build/target/release/iter-gateway /usr/local/bin/iter-gateway
EXPOSE 8090
ENTRYPOINT ["iter-gateway"]

# Data-prep image: the pipeline binary plus the external build toolchain
# (planetiler + JVM 21, osmium, go-pmtiles, zstd/unzip). Heavier than the slim
# runtime, so it gets its own base.
FROM eclipse-temurin:21-jre-jammy AS dataprep
ARG PLANETILER_VERSION=0.10.2
ARG PMTILES_VERSION=1.30.3
ARG OTP_VERSION=2.7.0
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl osmium-tool zstd unzip \
 && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL -o /opt/planetiler.jar \
      "https://github.com/onthegomap/planetiler/releases/download/v${PLANETILER_VERSION}/planetiler.jar" \
 && curl -fsSL -o /opt/otp.jar \
      "https://repo1.maven.org/maven2/org/opentripplanner/otp-shaded/${OTP_VERSION}/otp-shaded-${OTP_VERSION}.jar" \
 && curl -fsSL -o /tmp/pmtiles.tgz \
      "https://github.com/protomaps/go-pmtiles/releases/download/v${PMTILES_VERSION}/go-pmtiles_${PMTILES_VERSION}_Linux_x86_64.tar.gz" \
 && tar -xzf /tmp/pmtiles.tgz -C /usr/local/bin pmtiles \
 && rm /tmp/pmtiles.tgz
ENV DATA_DIR=/data REGIONS_DIR=/regions \
    PLANETILER_JAR=/opt/planetiler.jar OTP_JAR=/opt/otp.jar
COPY regions /regions

FROM dataprep AS pipeline
COPY --from=builder /build/target/release/iter-pipeline /usr/local/bin/iter-pipeline
ENTRYPOINT ["iter-pipeline"]

FROM runtime AS worker
COPY --from=builder /build/target/release/iter-worker /usr/local/bin/iter-worker
ENTRYPOINT ["iter-worker"]
