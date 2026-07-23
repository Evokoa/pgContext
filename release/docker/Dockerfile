ARG RUST_IMAGE=rust:1.96.0-bookworm@sha256:5e2214abe154fe26e39f64488952e5c991eeed1d6d6da7cc8381ae83927f0cfc
ARG POSTGRES_IMAGE=postgres:17-bookworm@sha256:4f736ae292687621d4dbe0d499ffd024a36bd2ee7d8ca6f2ccd4c800f047b394

FROM ${RUST_IMAGE} AS builder

ARG PG_MAJOR=17
ARG PGRX_VERSION=0.19.1

RUN case "${PG_MAJOR}" in \
      17|18) ;; \
      *) echo "pgContext supports PostgreSQL 17 and 18 (received PG_MAJOR=${PG_MAJOR})" >&2; exit 2 ;; \
    esac

RUN apt-get -o Acquire::Retries=3 update \
    && apt-get -o Acquire::Retries=3 install -y --no-install-recommends \
        ca-certificates curl gnupg lsb-release \
    && curl -fsSL https://www.postgresql.org/media/keys/ACCC4CF8.asc \
        | gpg --dearmor -o /usr/share/keyrings/postgresql.gpg \
    && echo "deb [signed-by=/usr/share/keyrings/postgresql.gpg] http://apt.postgresql.org/pub/repos/apt $(lsb_release -cs)-pgdg main" \
        > /etc/apt/sources.list.d/pgdg.list \
    && apt-get -o Acquire::Retries=3 update \
    && apt-get -o Acquire::Retries=3 install -y --no-install-recommends \
        postgresql-${PG_MAJOR} postgresql-server-dev-${PG_MAJOR} \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install cargo-pgrx --version ${PGRX_VERSION} --locked

WORKDIR /src
COPY . /src
RUN cargo pgrx init --pg${PG_MAJOR}=/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config \
    && cargo pgrx package -p context-pg \
        --pg-config=/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config \
        --out-dir=/package \
        --no-default-features \
        --features=pg${PG_MAJOR}

FROM ${POSTGRES_IMAGE}

ARG PG_MAJOR=17
ARG VERSION=0.1.0
ARG REVISION=unknown
LABEL org.opencontainers.image.source="https://github.com/evokoa/pgcontext" \
      org.opencontainers.image.description="PostgreSQL ${PG_MAJOR} with pgContext installed" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.postgresql.major="${PG_MAJOR}"

RUN server_major="$(postgres --version | awk '{print $3}' | cut -d. -f1)" \
    && test "${server_major}" = "${PG_MAJOR}" \
    || { echo "POSTGRES_IMAGE major ${server_major} does not match PG_MAJOR=${PG_MAJOR}" >&2; exit 2; }

COPY --from=builder /package/usr/share/postgresql/${PG_MAJOR}/extension/ \
    /usr/share/postgresql/${PG_MAJOR}/extension/
COPY --from=builder /package/usr/lib/postgresql/${PG_MAJOR}/lib/ \
    /usr/lib/postgresql/${PG_MAJOR}/lib/
COPY release/docker/init/01-pgcontext.sql /docker-entrypoint-initdb.d/

ENV POSTGRES_DB=pgcontext
HEALTHCHECK --interval=5s --timeout=3s --start-period=10s --retries=20 \
  CMD pg_isready -U "${POSTGRES_USER:-postgres}" -d "${POSTGRES_DB:-pgcontext}" || exit 1
