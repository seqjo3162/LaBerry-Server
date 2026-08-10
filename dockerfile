FROM rust:1.80-slim AS planner
WORKDIR /app
RUN cargo install cargo-chef

COPY ./server/src ./server/src
WORKDIR /app/server/src
RUN cargo chef prepare --recipe-path recipe.json

FROM rust:1.80-slim AS builder
WORKDIR /app
RUN cargo install cargo-chef
COPY --from=planner /app/server/src/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path reciper.json

COPY ./server/src ./server/src
COPY ./server/static ./server/static
WORKDIR /app/server/src
RUN cargo build --release

FROM debian:bookworm-slim AS runtime
WORKDIR /app
COPY --from=builder /app/server/src/target/release/Messenger /app/Messenger
COPY ./server/static /app/server/static
EXPOSE 5001
CMD ["./Messenger"]