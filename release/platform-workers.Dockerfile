FROM node:24.21.0-bookworm-slim@sha256:0e0ff40c39bc087845bfb27465a0df4ea419520094bc35842ff83dd8cbe6f9b6

ENV NODE_ENV=production
ARG LIGHTSPEED_RELEASE_VERSION=0.0.0
ARG LIGHTSPEED_GIT_SHA=unknown
ENV LIGHTSPEED_RELEASE_VERSION=$LIGHTSPEED_RELEASE_VERSION
LABEL org.opencontainers.image.title="Lightspeed connector host" \
      org.opencontainers.image.version=$LIGHTSPEED_RELEASE_VERSION \
      org.opencontainers.image.revision=$LIGHTSPEED_GIT_SHA \
      org.opencontainers.image.source="https://github.com/smartcomputer-ai/lightspeed"
WORKDIR /app
ADD --chown=node:node dist/runtime/platform-workers.tar.gz /app/
USER node
ENTRYPOINT ["node", "--import", "tsx", "platform/connectors/src/host/main.ts"]
