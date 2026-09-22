FROM rust:1.97.1-slim-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src ./src
COPY contracts ./contracts
COPY assets ./assets
RUN cargo build --locked --release --bin apiosk-gateway-v2

FROM rust:1.97.1-slim-bookworm AS executor-builder
WORKDIR /executor
COPY legacy-executor ./
RUN cargo build --locked --release \
    --bin apiosk-gateway \
    --bin check_dutch_public_sources \
    --bin check_eu_public_sources \
    --bin check_dutch_identifier_links \
    --bin check_domain_intelligence \
    --bin check_uk_vat \
    --bin reconciliation_worker

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates poppler-utils \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/apiosk-gateway-v2 /usr/local/bin/apiosk-gateway-v2
COPY --from=executor-builder /executor/target/release/apiosk-gateway /usr/local/bin/apiosk-payment-executor
COPY --from=executor-builder /executor/target/release/reconciliation_worker /usr/local/bin/apiosk-reconciliation-worker
COPY --from=executor-builder /executor/target/release/check_dutch_public_sources /usr/local/bin/check-dutch-public-sources
COPY --from=executor-builder /executor/target/release/check_dutch_identifier_links /usr/local/bin/check-dutch-identifier-links
COPY --from=executor-builder /executor/target/release/check_eu_public_sources /usr/local/bin/check-eu-public-sources
COPY --from=executor-builder /executor/target/release/check_domain_intelligence /usr/local/bin/check-domain-intelligence
COPY --from=executor-builder /executor/target/release/check_uk_vat /usr/local/bin/check-uk-vat
COPY deploy/certs/supabase-prod-ca-2021.crt /etc/ssl/certs/supabase-prod-ca-2021.crt
COPY entrypoint.sh /usr/local/bin/apiosk-gateway-v2-entrypoint
RUN chmod 0755 /usr/local/bin/apiosk-gateway-v2-entrypoint
USER 10001:10001
ENV BIND_ADDRESS=0.0.0.0:8082
EXPOSE 8082
CMD ["apiosk-gateway-v2-entrypoint"]
