# syntax=docker/dockerfile:1.7
# The Kodehub server-side image: kodehub-server (API + orchestrator), kodehub-scheduler,
# kodehub-storage, kodehub-worker (build / test / git containers) and the kodehub CLI/MCP.
# Pilot: one image for every server-side role; the agent image is separate and tiny (S8).
FROM rust:1-bullseye AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/cargo-target \
    CARGO_TARGET_DIR=/cargo-target cargo build --release --bins --locked \
 && mkdir -p /out \
 && for b in kodehub-server kodehub kodehub-storage kodehub-scheduler kodehub-worker; do \
      if [ -f /cargo-target/release/$b ]; then cp /cargo-target/release/$b /out/; fi; done

FROM rust:1-bullseye AS toolchain

FROM python:3.12-slim
# a C toolchain + git for building user code (Rust crates, PyO3 extensions) and Git import/export
RUN apt-get update \
 && apt-get install -y --no-install-recommends gcc libc6-dev git ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && pip install --no-cache-dir uv==0.8.17
COPY --from=toolchain /usr/local/cargo /usr/local/cargo
COPY --from=toolchain /usr/local/rustup /usr/local/rustup
ENV CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup PATH=/usr/local/cargo/bin:$PATH
COPY --from=build /out/ /usr/local/bin/
COPY app/dist /app/dist
COPY examples /app/examples
COPY crates/kodehub-agents/prompts /app/prompts
COPY crates/kodehub-agents/recordings /app/recordings
COPY scripts /app/scripts
RUN useradd -m -u 10001 kodehub && mkdir -p /data && chown kodehub /data
USER kodehub
ENV KODEHUB_HOME=/data/kode KODEHUB_STATIC_DIR=/app/dist KODEHUB_BIND=0.0.0.0:7878
WORKDIR /data
EXPOSE 7878 9000
CMD ["kodehub-server"]
