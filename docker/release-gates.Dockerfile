ARG RUST_VERSION=1.96.0
FROM rust:${RUST_VERSION}-bookworm

ARG PG_MAJOR=17
ARG PGRX_VERSION=0.19.1

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        gnupg \
        lsb-release \
        postgresql-common \
    && /usr/share/postgresql-common/pgdg/apt.postgresql.org.sh -y \
    && apt-get install -y --no-install-recommends \
        clang \
        libclang-dev \
        pkg-config \
        postgresql-${PG_MAJOR} \
        postgresql-server-dev-${PG_MAJOR} \
    && rm -rf /var/lib/apt/lists/*

RUN rustup component add clippy rustfmt \
    && cargo install cargo-pgrx --version ${PGRX_VERSION} --locked

WORKDIR /workspace

ENV PG_MAJOR=${PG_MAJOR}
ENV PG_FEATURE=pg${PG_MAJOR}
ENV PG_CONFIG=/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config

CMD cargo pgrx init --pg${PG_MAJOR} ${PG_CONFIG} \
    && cargo fmt --check \
    && cargo clippy --workspace --exclude context-pg --all-targets --all-features -- -D warnings \
    && cargo test --workspace --exclude context-pg --all-features \
    && cargo check -p context-pg --no-default-features --features ${PG_FEATURE} \
    && mkdir -p target/release-sql \
    && cargo pgrx schema -p context-pg pg${PG_MAJOR} --out target/release-sql/pg${PG_MAJOR}.sql
