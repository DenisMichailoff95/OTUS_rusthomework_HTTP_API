# syntax=docker/dockerfile:1
FROM rust:latest AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev libprotobuf-dev protobuf-compiler openssl && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release -p shorty-server

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates openssl && rm -rf /var/lib/apt/lists/*
RUN groupadd -r appuser && useradd -r -g appuser -d /app -s /sbin/nologin appuser
WORKDIR /app
COPY --from=builder /app/target/release/shorty-server /usr/local/bin/shorty-server
COPY scripts/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh && chown appuser:appuser /usr/local/bin/docker-entrypoint.sh
RUN mkdir -p /app/keys && chown -R appuser:appuser /app
USER appuser
EXPOSE 8080 50051
ENTRYPOINT ["docker-entrypoint.sh"]
CMD ["shorty-server"]
