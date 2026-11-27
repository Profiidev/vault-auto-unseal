ARG TARGETARCH
# If TARGETARCH is amd64, result is x86_64. If arm64, result is aarch64.
ARG RUST_ARCH=${TARGETARCH/amd64/x86_64}
ARG RUST_ARCH=${RUST_ARCH/arm64/aarch64}
ARG TARGET=${RUST_ARCH}-unknown-linux-gnu
ARG RUSTFLAGS="-C target-feature=+crt-static"
ARG BIN=vault-auto-unseal

FROM ghcr.io/profiidev/images/rust-gnu-builder:main@sha256:5e71dc6dc6f1263f670e506e5a1231d6069acce53c9859b98d9dc3fe2e8d4f2c AS planner

ARG BIN

COPY ./Cargo.toml ./Cargo.lock ./

RUN cargo chef prepare --recipe-path recipe.json --bin $BIN

FROM ghcr.io/profiidev/images/rust-gnu-builder:main@sha256:5e71dc6dc6f1263f670e506e5a1231d6069acce53c9859b98d9dc3fe2e8d4f2c AS builder

ARG BIN
ARG TARGET
ARG RUSTFLAGS

COPY --from=planner /app/recipe.json .

RUN \
  --mount=type=cache,target=/usr/local/cargo/registry \
  --mount=type=cache,target=/app/target \
  cargo chef cook --release --target $TARGET

COPY ./src ./src
COPY ./Cargo.toml ./Cargo.lock ./

RUN \
  --mount=type=cache,target=/usr/local/cargo/registry \
  --mount=type=cache,target=/app/target \
  cargo build --release --target $TARGET --bin $BIN \
  && mv ./target/$TARGET/release/$BIN ./app

FROM scratch

COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=builder /app/app /usr/local/bin/vault-auto-unseal

USER 65532:65532

ENTRYPOINT ["vault-auto-unseal"]
