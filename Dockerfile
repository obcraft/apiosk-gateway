# syntax=docker/dockerfile:1

# One workspace, one server binary, one compiler version.
FROM rust:1.97.1-slim-bookworm AS chef
RUN cargo install cargo-chef --locked --version 0.1.78

FROM chef AS gateway-planner
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS gateway-dependencies
WORKDIR /build
COPY --from=gateway-planner /build/recipe.json ./recipe.json
# target/ is deliberately a normal layer: mode=max exports it to CI cache.
# A cache mount alone would disappear on a fresh GitHub Actions runner.
RUN cargo chef cook --locked --release --recipe-path recipe.json \
    --bin apiosk-gateway-v2 --bin check_brightdata_linkedin

FROM gateway-dependencies AS builder
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src ./src
COPY contracts ./contracts
COPY assets ./assets
COPY perf ./perf
RUN cargo build --locked --release --bin apiosk-gateway-v2 --bin check_brightdata_linkedin

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates poppler-utils \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/apiosk-gateway-v2 /usr/local/bin/apiosk-gateway-v2
COPY --from=builder /build/target/release/check_brightdata_linkedin /usr/local/bin/check-brightdata-linkedin
COPY deploy/certs/supabase-prod-ca-2021.crt /etc/ssl/certs/supabase-prod-ca-2021.crt
COPY entrypoint.sh /usr/local/bin/apiosk-gateway-v2-entrypoint
RUN chmod 0755 /usr/local/bin/apiosk-gateway-v2-entrypoint
# Stamp only the final image; a revision must never invalidate compilation.
ARG GIT_SHA=unknown
ENV GATEWAY_GIT_SHA=$GIT_SHA
LABEL org.opencontainers.image.revision=$GIT_SHA \
      org.opencontainers.image.source="https://github.com/obcraft/apiosk-gateway"
USER 10001:10001
ENV BIND_ADDRESS=0.0.0.0:8082
EXPOSE 8082
CMD ["apiosk-gateway-v2-entrypoint"]
