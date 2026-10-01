ARG TARGET=x86_64-unknown-linux-gnu
ARG RUSTFLAGS="-C target-feature=+crt-static"
ARG BIN=vault-auto-unseal

FROM ghcr.io/profiidev/images/rust-gnu-builder:main@sha256:58cb837025f0f8eb4b256b8a2ef0ef93958961ba6835123578d8b0c338a974a8 AS planner

ARG BIN
ARG TARGET
ARG RUSTFLAGS

COPY ./Cargo.toml ./Cargo.lock ./

RUN cargo chef prepare --recipe-path recipe.json --bin $BIN

FROM ghcr.io/profiidev/images/rust-gnu-builder:main@sha256:58cb837025f0f8eb4b256b8a2ef0ef93958961ba6835123578d8b0c338a974a8 AS builder

ARG BIN
ARG TARGET
ARG RUSTFLAGS

COPY --from=planner /app/recipe.json .

RUN cargo chef cook --release --target $TARGET

COPY ./src ./src
COPY ./Cargo.toml ./Cargo.lock ./

RUN cargo build --release --target $TARGET --bin $BIN
RUN mv ./target/$TARGET/release/$BIN ./app

FROM alpine@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6

RUN addgroup -S user
RUN adduser -G user -S user

WORKDIR /app
RUN chown -R user:user /app

USER user

COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/

COPY --from=builder /app/app /usr/local/bin/

CMD ["app"]