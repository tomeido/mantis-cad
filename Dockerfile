# syntax=docker/dockerfile:1.7@sha256:a57df69d0ea827fb7266491f2813635de6f17269be881f696fbfdf2d83dda33e

ARG TRUNK_VERSION=0.21.14
ARG MANTIS_WEB_BASE_PATH=""
ARG MANTIS_WEB_PUBLIC_URL=/

FROM --platform=$BUILDPLATFORM rust:1.97.1-bookworm@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97 AS web-builder

ARG TRUNK_VERSION
WORKDIR /src

RUN rustup target add wasm32-unknown-unknown \
    && cargo install --locked --version "${TRUNK_VERSION}" trunk

ARG MANTIS_GIT_SHA=unknown
ARG MANTIS_WEB_BASE_PATH
ARG MANTIS_WEB_PUBLIC_URL
ENV MANTIS_GIT_SHA=${MANTIS_GIT_SHA}

COPY . .

RUN --mount=type=cache,id=mantis-web-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=mantis-web-target,target=/src/target,sharing=locked \
    cd crates/mantis-app \
    && trunk build index.html \
      --release \
      --locked \
      --dist /out/dist \
      --public-url "${MANTIS_WEB_PUBLIC_URL}"

FROM rust:1.97.1-bookworm@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97 AS server-builder

ARG MANTIS_GIT_SHA=unknown
ARG TARGETARCH
ENV MANTIS_GIT_SHA=${MANTIS_GIT_SHA}
WORKDIR /src

COPY . .

RUN --mount=type=cache,id=mantis-native-registry-${TARGETARCH},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=mantis-native-target-${TARGETARCH},target=/src/target,sharing=locked \
    cargo build --locked --release -p mantis-server -p mantis-admin \
    && install -Dm755 target/release/mantis-server /out/bin/mantis-server \
    && install -Dm755 target/release/mantis-admin /out/bin/mantis-admin

FROM debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241 AS runtime

ARG MANTIS_GIT_SHA=unknown

LABEL org.opencontainers.image.title="MantisCAD" \
      org.opencontainers.image.description="Parametric CAD web app and signed collaboration server" \
      org.opencontainers.image.source="https://github.com/tomeido/mantis-cad" \
      org.opencontainers.image.revision="${MANTIS_GIT_SHA}" \
      org.opencontainers.image.licenses="MIT"

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 mantis \
    && useradd --uid 10001 --gid mantis --create-home --home-dir /home/mantis --shell /bin/bash mantis \
    && install -d -o mantis -g mantis -m 0700 /home/mantis/.ssh \
    && install -d -o mantis -g mantis -m 0750 /data \
    && install -d -o root -g root -m 0755 /app/dist

COPY --from=server-builder --chown=root:root /out/bin/mantis-server /usr/local/bin/mantis-server
COPY --from=server-builder --chown=root:root /out/bin/mantis-admin /usr/local/bin/mantis-admin
COPY --from=web-builder --chown=root:root /out/dist/ /app/dist/
COPY --chown=root:root LICENSE /usr/share/licenses/mantis-cad/LICENSE

ENV PORT=7878 \
    MANTIS_DATA_DIR=/data \
    MANTIS_DIST_DIR=/app/dist \
    MANTIS_OPERATOR_KEYS="" \
    MANTIS_ALLOWED_ORIGINS="" \
    MANTIS_MAX_PROJECT_BYTES=25165824

VOLUME ["/data"]
EXPOSE 7878

USER mantis:mantis

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl --fail --silent --show-error "http://127.0.0.1:${PORT}/readyz" >/dev/null || exit 1

ENTRYPOINT ["/usr/local/bin/mantis-server"]
