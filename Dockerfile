ARG TARGET=x86_64-unknown-linux-gnu
ARG RUSTFLAGS="-C target-feature=+crt-static"
ARG BIN=vault-auto-unseal

FROM ghcr.io/profiidev/images/rust-gnu-builder:main@sha256:bd1ae1e4cf5ba1b9225619dec7b7ad4e040ed742f5898c6e24ad3a298b8fbe3f AS planner

ARG BIN
ARG TARGET
ARG RUSTFLAGS

COPY ./Cargo.toml ./Cargo.lock ./

RUN cargo chef prepare --recipe-path recipe.json --bin $BIN

FROM ghcr.io/profiidev/images/rust-gnu-builder:main@sha256:bd1ae1e4cf5ba1b9225619dec7b7ad4e040ed742f5898c6e24ad3a298b8fbe3f AS builder

ARG BIN
ARG TARGET
ARG RUSTFLAGS

COPY --from=planner /app/recipe.json .

RUN cargo chef cook --release --target $TARGET

COPY ./src ./src
COPY ./Cargo.toml ./Cargo.lock ./

RUN cargo build --release --target $TARGET --bin $BIN
RUN mv ./target/$TARGET/release/$BIN ./app

FROM alpine@sha256:28bd5fe8b56d1bd048e5babf5b10710ebe0bae67db86916198a6eec434943f8b

RUN addgroup -S user
RUN adduser -G user -S user

WORKDIR /app
RUN chown -R user:user /app

USER user

COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/

COPY --from=builder /app/app /usr/local/bin/

CMD ["app"]