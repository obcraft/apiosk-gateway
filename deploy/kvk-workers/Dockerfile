FROM rust:1.97.1-slim-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src ./src
RUN cargo build --locked --release -p kvk-accounts --bin apiosk-kvk-worker

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/apiosk-kvk-worker /usr/local/bin/apiosk-kvk-worker
COPY deploy/certs/supabase-prod-ca-2021.crt /etc/ssl/certs/supabase-prod-ca-2021.crt
USER 10001:10001
ENV RUST_LOG=info
CMD ["/usr/local/bin/apiosk-kvk-worker"]
