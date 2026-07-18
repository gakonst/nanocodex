FROM rust:1.88-bookworm

WORKDIR /app
COPY Cargo.toml ./
COPY src ./src

RUN cargo test --quiet
