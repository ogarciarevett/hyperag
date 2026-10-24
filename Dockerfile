# The Hyperag service (HTTP API, MCP at /mcp, embedder and re-ranker in process).
FROM rust:1.99-slim-trixie@sha256:aab42391a986424e22d6a3ec14f37bc6dd3dacff4d457ca7cccd781e02297cc6 AS build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config ca-certificates g++ && rm -rf /var/lib/apt/lists/*
WORKDIR /src
# Build parallelism, kept low on shared machines: docker build --build-arg CARGO_BUILD_JOBS=6
ARG CARGO_BUILD_JOBS=6
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY README.md LICENSE NOTICE ./
RUN cargo build --release -p hyperag && cp target/release/hyperag /hyperag

FROM debian:trixie-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl libstdc++6 && rm -rf /var/lib/apt/lists/* \
 && useradd --uid 10001 --create-home hyperag \
 && mkdir -p /home/hyperag/models /config && chown hyperag:hyperag /home/hyperag/models /config
COPY --from=build /hyperag /usr/local/bin/hyperag
USER hyperag
ENV HYPERAG_HOST=0.0.0.0 HYPERAG_PORT=7480 HYPERAG_MODEL_CACHE=/home/hyperag/models
EXPOSE 7480
HEALTHCHECK --interval=5s --timeout=3s --retries=60 CMD curl -sf http://127.0.0.1:7480/healthz || exit 1
ENTRYPOINT ["hyperag"]
CMD ["serve"]
