FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app


FROM chef AS planner

COPY Cargo.toml Cargo.lock ./
RUN \
  mkdir -p src \
  && echo "pub fn main() { println!(\"Hello, world!\"); }" > src/lib.rs \
  && cargo chef prepare --recipe-path recipe.json


FROM chef AS builder

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY ./ ./
RUN cargo install --root dist --path .


FROM debian:unstable-slim AS runtime

WORKDIR /app
COPY --from=builder /app/dist/bin/vidsort /usr/local/bin/
ENTRYPOINT ["/usr/local/bin/vidsort"]
