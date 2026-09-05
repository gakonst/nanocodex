FROM rust:1.88-bookworm

WORKDIR /app
COPY Cargo.toml README.md ./
COPY src ./src

RUN cargo test --quiet
