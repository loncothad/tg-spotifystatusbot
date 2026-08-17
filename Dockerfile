# syntax=docker/dockerfile:1.7

FROM rust:1-bookworm AS builder
WORKDIR /src

RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY xtask ./xtask
COPY .cargo ./.cargo

RUN cargo build --release -p tg-spotifystatusbot \
    && strip target/release/tg-spotifystatusbot

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /var/lib/tg-spotifystatusbot --create-home --uid 10001 bot

COPY --from=builder /src/target/release/tg-spotifystatusbot /usr/local/bin/tg-spotifystatusbot

USER bot
WORKDIR /var/lib/tg-spotifystatusbot
ENV REDB_PATH=/var/lib/tg-spotifystatusbot/bot.redb \
    HTTP_BIND=0.0.0.0:8080 \
    RUST_LOG=info

EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/tg-spotifystatusbot"]
