# syntax=docker/dockerfile:1

# Builder runs on musl (alpine) natively so the release binary is already
# statically linked against musl libc — no cross-compilation, no zig, no
# separate --target juggling. sqlx's "sqlite" feature statically compiles
# bundled sqlite3.c via `cc` (see Cargo.lock: libsqlite3-sys has no system
# sqlite dependency), so musl-dev + gcc are the only extra packages needed.
FROM rust:1-alpine AS builder
RUN apk add --no-cache musl-dev gcc

WORKDIR /app

# Cache dependency compilation in its own layer, separate from application
# source, so `cargo build` only recompiles budget's own code on source changes.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && \
    cargo build --release --locked && \
    rm -rf src target/release/deps/budget-*

COPY migrations ./migrations
COPY src ./src
RUN cargo build --release --frozen

# ca-certificates: reqwest/rustls reads the system trust store (via
# rustls-native-certs) to verify Plaid's TLS certs at runtime.
FROM alpine:3.21 AS runtime
RUN apk add --no-cache ca-certificates && \
    addgroup -S app && adduser -S app -G app

WORKDIR /app
COPY --from=builder /app/target/release/budget /usr/local/bin/budget
COPY config ./config

RUN mkdir -p /app/data && chown -R app:app /app
USER app

ENV RUN_MODE=production
EXPOSE 3000

CMD ["/usr/local/bin/budget"]
