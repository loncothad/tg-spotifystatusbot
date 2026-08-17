set dotenv-load := false

export RUST_BACKTRACE := "1"

default:
    @just --list

build:
    cargo build --workspace

release:
    cargo build --workspace --release -p tg-spotifystatusbot

test:
    cargo test --workspace

fmt:
    cargo fmt --all

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

run:
    cargo run -p tg-spotifystatusbot

render-example:
    cargo xtask render-example

docker:
    docker compose build

docker-up:
    docker compose up -d --build

docker-down:
    docker compose down

check: fmt clippy test
