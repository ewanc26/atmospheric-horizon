# syntax=docker/dockerfile:1

FROM rust:1.95-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --locked --release -p atmospheric-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 atmospheric \
    && useradd --uid 10001 --gid atmospheric --home-dir /data --shell /usr/sbin/nologin atmospheric \
    && mkdir -p /data \
    && chown atmospheric:atmospheric /data

COPY --from=build /src/target/release/atmospheric-server /usr/local/bin/atmospheric-server

ENV ATMOSPHERIC_SERVER_ADDR=0.0.0.0:8787
ENV ATMOSPHERIC_SERVER_DATA_DIR=/data
EXPOSE 8787
VOLUME ["/data"]
USER atmospheric
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=5 \
    CMD curl -fsS http://127.0.0.1:8787/healthz >/dev/null || exit 1

ENTRYPOINT ["/usr/local/bin/atmospheric-server"]
