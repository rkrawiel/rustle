# Builds a musl-linked release binary and ships it in a minimal runtime image. The
# builder stage is where the brief's release-binary size budget is measured (see
# `deploy/README.md` and `PLAN.md`'s budget table): `rust:1-alpine` targets musl libc
# natively, so this is a normal `cargo build --release`, not a cross-compile.

# syntax=docker/dockerfile:1

FROM rust:1-alpine AS builder
# musl-dev, gcc and make: `ring` (the sole rustls/sqlx crypto provider — see
# `scripts/dep-gate.ps1`) builds a small amount of C and needs a toolchain for it, even
# linked against musl.
RUN apk add --no-cache musl-dev gcc make

WORKDIR /app
COPY . .

# The committed `.sqlx` cache makes this buildable with no database reachable from inside
# the image build; `.cargo/config.toml` already sets `SQLX_OFFLINE=true`, repeated here so
# the build does not depend on that file being read correctly.
ENV SQLX_OFFLINE=true
RUN cargo build --release --locked

# ---------------------------------------------------------------------------------------

FROM alpine:3.20

# ca-certificates: outbound feed fetches are rustls over HTTPS and need a root store to
# validate publishers' certificates. tzdata: so RUSTLE_LOG_LEVEL's timestamps and any
# feed-date handling observe the container's configured TZ rather than only ever UTC.
RUN apk add --no-cache ca-certificates tzdata \
    && addgroup -S rustle \
    && adduser -S -G rustle -H -s /sbin/nologin rustle

COPY --from=builder /app/target/release/rustle /usr/local/bin/rustle

USER rustle
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/rustle"]
