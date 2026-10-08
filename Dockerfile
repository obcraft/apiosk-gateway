FROM rust:1.97.1-slim-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src ./src
COPY contracts ./contracts
COPY assets ./assets
COPY perf ./perf
RUN cargo build --locked --release --bin apiosk-gateway-v2 --bin check_brightdata_linkedin

FROM rust:1.97.1-slim-bookworm AS executor-builder
WORKDIR /executor
COPY legacy-executor ./
RUN cargo build --locked --release --bin apiosk-gateway

FROM debian:bookworm-slim
# Stamped at deploy time: `fly deploy --build-arg GIT_SHA=$(git rev-parse --short HEAD)`.
ARG GIT_SHA=unknown
ENV GATEWAY_GIT_SHA=$GIT_SHA
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates poppler-utils \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/apiosk-gateway-v2 /usr/local/bin/apiosk-gateway-v2
COPY --from=builder /build/target/release/check_brightdata_linkedin /usr/local/bin/check-brightdata-linkedin
COPY --from=executor-builder /executor/target/release/apiosk-gateway /usr/local/bin/apiosk-payment-executor
COPY deploy/certs/supabase-prod-ca-2021.crt /etc/ssl/certs/supabase-prod-ca-2021.crt
COPY entrypoint.sh /usr/local/bin/apiosk-gateway-v2-entrypoint
RUN chmod 0755 /usr/local/bin/apiosk-gateway-v2-entrypoint
USER 10001:10001
ENV BIND_ADDRESS=0.0.0.0:8082
EXPOSE 8082
CMD ["apiosk-gateway-v2-entrypoint"]
