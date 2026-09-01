ARG TARGET=x86_64-unknown-linux-gnu
ARG RUSTFLAGS="-C target-feature=+crt-static"
ARG BIN=vault-auto-unseal

FROM ghcr.io/profiidev/images/rust-gnu-builder:main@sha256:ff57874d1ac77b2bde727c89e113ac21eb6b001f77c859fed6b31f008a1acfd8 AS planner

ARG BIN
ARG TARGET
ARG RUSTFLAGS

COPY ./Cargo.toml ./Cargo.lock ./

RUN cargo chef prepare --recipe-path recipe.json --bin $BIN

FROM ghcr.io/profiidev/images/rust-gnu-builder:main@sha256:ff57874d1ac77b2bde727c89e113ac21eb6b001f77c859fed6b31f008a1acfd8 AS builder

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