# syntax=docker/dockerfile:1
# Builds the native binary: runs the test suite, then compiles an optimized release.
FROM rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663 AS builder
WORKDIR /app
# Parallel compile jobs; lower it on machines with little memory.
ARG CARGO_JOBS=4
ARG LAMBDA_FEATURES=memory-runtime,router-http
# Set to 0 to skip the test suite and only build.
ARG RUN_TESTS=1
ENV CARGO_BUILD_JOBS=$CARGO_JOBS CARGO_PROFILE_DEV_DEBUG=0
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
COPY migrations ./migrations
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/app/target \
    cargo fetch --locked && \
    if [ "$RUN_TESTS" = "1" ]; then cargo test --offline --locked --features "$LAMBDA_FEATURES"; fi && \
    cargo build --release --offline --locked --features "$LAMBDA_FEATURES" && \
    mkdir -p /out && cp target/release/lambda-memory /out/lambda-memory

# One process, one embedded SurrealKV store, one volume.
FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /var/lib/lambda-memory && chown 65534:65534 /var/lib/lambda-memory
COPY --from=builder /out/lambda-memory /usr/local/bin/lambda-memory
ENV LAMBDA_MEMORY_DATA_DIR=/var/lib/lambda-memory/store
VOLUME ["/var/lib/lambda-memory"]
USER 65534:65534
ENTRYPOINT ["lambda-memory"]
CMD ["--help"]
