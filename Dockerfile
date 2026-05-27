# syntax=docker/dockerfile:1.7

# ---- builder ----
FROM rust:1.94-alpine3.20 AS builder

RUN apk add --no-cache musl-dev pkgconfig

WORKDIR /src

# Pre-cache the dependency graph: copy manifests, build a stub.
COPY Cargo.toml Cargo.lock rust-toolchain.toml rustfmt.toml ./
COPY crates/tnl-protocol/Cargo.toml crates/tnl-protocol/Cargo.toml
COPY crates/tnld/Cargo.toml         crates/tnld/Cargo.toml
COPY crates/tnl/Cargo.toml          crates/tnl/Cargo.toml
COPY crates/tnl-e2e/Cargo.toml      crates/tnl-e2e/Cargo.toml
RUN set -eux; \
    for c in tnl-protocol tnl-e2e; do \
        mkdir -p crates/$c/src && echo 'pub fn _stub() {}' > crates/$c/src/lib.rs; \
    done; \
    mkdir -p crates/tnld/src crates/tnl/src; \
    echo 'fn main() {}' > crates/tnld/src/main.rs; \
    echo 'fn main() {}' > crates/tnl/src/main.rs; \
    echo 'pub fn _stub() {}' > crates/tnld/src/lib.rs; \
    echo 'pub fn _stub() {}' > crates/tnl/src/lib.rs; \
    cargo build --release --bin tnld; \
    rm -rf crates/*/src

# Real sources, real build.
COPY crates/ crates/
RUN set -eux; \
    touch crates/tnl-protocol/src/lib.rs crates/tnld/src/main.rs; \
    cargo build --release --bin tnld; \
    strip target/release/tnld

# ---- runtime ----
FROM alpine:3.20

RUN apk add --no-cache ca-certificates tini && \
    addgroup -S tnld && adduser -S -G tnld -H -s /sbin/nologin tnld

COPY --from=builder /src/target/release/tnld /usr/local/bin/tnld

USER tnld
EXPOSE 7777
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
    CMD ["/usr/local/bin/tnld", "healthcheck", "--config", "/etc/tnld/config.toml"]
ENTRYPOINT ["/sbin/tini", "--", "/usr/local/bin/tnld"]
CMD ["serve", "--config", "/etc/tnld/config.toml"]
