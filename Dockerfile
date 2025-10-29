ARG BIN=vault-auto-unseal

FROM ghcr.io/profiidev/images/rust-musl-builder:main AS planner

ARG BIN
ENV BIN=$BIN

COPY apps/vault-auto-unseal/Cargo.toml ./Cargo.lock ./

RUN cargo chef prepare --recipe-path recipe.json --bin $BIN

FROM ghcr.io/profiidev/images/rust-musl-builder:main AS builder

ARG BIN
ENV BIN=$BIN

COPY --from=planner /app/recipe.json .

RUN cargo chef cook --release

COPY apps/vault-auto-unseal/src ./src
COPY apps/vault-auto-unseal/Cargo.toml ./Cargo.lock ./

RUN cargo build --release --bin $BIN
RUN mv ./target/x86_64-unknown-linux-musl/release/$BIN ./app

FROM alpine

RUN addgroup -S user
RUN adduser -G user -S user

WORKDIR /app
RUN chown -R user:user /app

USER user

COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/

COPY --from=builder /app/app /usr/local/bin/

CMD ["app"]