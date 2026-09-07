FROM rust:1.97.1-slim-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src ./src
COPY contracts ./contracts
RUN cargo build --locked --release --bin apiosk-gateway-v2

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/apiosk-gateway-v2 /usr/local/bin/apiosk-gateway-v2
COPY deploy/certs/supabase-prod-ca-2021.crt /etc/ssl/certs/supabase-prod-ca-2021.crt
USER 10001:10001
ENV BIND_ADDRESS=0.0.0.0:8082
EXPOSE 8082
CMD ["apiosk-gateway-v2"]
