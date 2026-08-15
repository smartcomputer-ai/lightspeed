FROM debian:12-slim@sha256:362e64223cc0da95422b3b13c045186fc0a81250e765d31c025fbddf257f6143

ARG LIGHTSPEED_RELEASE_VERSION=0.0.0
ARG LIGHTSPEED_GIT_SHA=unknown
LABEL org.opencontainers.image.title="Lightspeed runtime" \
      org.opencontainers.image.version=$LIGHTSPEED_RELEASE_VERSION \
      org.opencontainers.image.revision=$LIGHTSPEED_GIT_SHA \
      org.opencontainers.image.source="https://github.com/smartcomputer-ai/lightspeed"

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates ffmpeg \
    && rm -rf /var/lib/apt/lists/*

COPY dist/bin/lightspeed-server /usr/local/bin/lightspeed-server
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/lightspeed-server"]
