FROM node:24.13.0-bookworm-slim@sha256:46feb5752989c05b8606e6323fbbc3db667d14ade1c24f5d0d44d9ca9909d607

ENV NODE_ENV=production
ARG LIGHTSPEED_RELEASE_VERSION=0.0.0
ARG LIGHTSPEED_GIT_SHA=unknown
ENV LIGHTSPEED_RELEASE_VERSION=$LIGHTSPEED_RELEASE_VERSION
LABEL org.opencontainers.image.title="Lightspeed platform" \
      org.opencontainers.image.version=$LIGHTSPEED_RELEASE_VERSION \
      org.opencontainers.image.revision=$LIGHTSPEED_GIT_SHA \
      org.opencontainers.image.source="https://github.com/smartcomputer-ai/lightspeed"
WORKDIR /app
ADD --chown=node:node dist/runtime/platform.tar.gz /app/
USER node
EXPOSE 3000
CMD ["node", "--import", "tsx", "platform/server/src/main.ts"]
