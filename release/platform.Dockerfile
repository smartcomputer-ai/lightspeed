FROM node:24.22.2-trixie-slim@sha256:0820eaa85e07d0490446dd01d14707e17e6108a36988cf374899cd057eb3812d

# Dependencies are installed and verified before image assembly. The server
# starts with Node directly and does not need package managers at runtime.
RUN rm -rf /usr/local/lib/node_modules/npm /opt/yarn-* \
    && rm -f /usr/local/bin/npm /usr/local/bin/npx \
        /usr/local/bin/yarn /usr/local/bin/yarnpkg

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
# Numeric IDs let Kubernetes verify runAsNonRoot from image metadata.
USER 1000:1000
EXPOSE 3000
CMD ["node", "--import", "tsx", "platform/backend/src/main.ts"]
