FROM rust:1.85-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --bin bot

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/bot /usr/local/bin/bot
WORKDIR /app
ENTRYPOINT ["bot"]
CMD ["--help"]

