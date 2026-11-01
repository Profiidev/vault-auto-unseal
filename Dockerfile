ARG TARGET=x86_64-unknown-linux-gnu
ARG RUSTFLAGS="-C target-feature=+crt-static"
ARG BIN=vault-auto-unseal

FROM ghcr.io/profiidev/images/rust-gnu-builder:main@sha256:ec481bc66b89eb0d7e489013b2b0396155c9f863032ac8be3e75362e6c86be91 AS planner

ARG BIN
ARG TARGET
ARG RUSTFLAGS

COPY ./Cargo.toml ./Cargo.lock ./

RUN cargo chef prepare --recipe-path recipe.json --bin $BIN

FROM ghcr.io/profiidev/images/rust-gnu-builder:main@sha256:ec481bc66b89eb0d7e489013b2b0396155c9f863032ac8be3e75362e6c86be91 AS builder

ARG BIN
ARG TARGET
ARG RUSTFLAGS

COPY --from=planner /app/recipe.json .

RUN cargo chef cook --release --target $TARGET

COPY ./src ./src
COPY ./Cargo.toml ./Cargo.lock ./

RUN cargo build --release --target $TARGET --bin $BIN
RUN mv ./target/$TARGET/release/$BIN ./app

FROM alpine@sha256:be2656061b536c1883c4034790e21d0a09c5aaaf51968b89368a3c0371ea2c9a

RUN addgroup -S user
RUN adduser -G user -S user

WORKDIR /app
RUN chown -R user:user /app

USER user

COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/

COPY --from=builder /app/app /usr/local/bin/

CMD ["app"]