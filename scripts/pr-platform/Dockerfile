# Rebuilt only when this file or the exact locked Playwright version changes.
FROM node:24-bookworm AS node
FROM rust:1.95-bookworm
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules/npm /usr/local/lib/node_modules/npm
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
    && ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx
RUN sed -i 's|http://deb.debian.org|https://deb.debian.org|g' /etc/apt/sources.list.d/debian.sources \
    && apt-get -o Acquire::Retries=3 update && apt-get -o Acquire::Retries=3 install -y --no-install-recommends \
    ca-certificates git pkg-config build-essential libwebkit2gtk-4.1-dev \
    libappindicator3-dev librsvg2-dev patchelf xdg-utils \
    && rm -rf /var/lib/apt/lists/*
ARG PLAYWRIGHT_VERSION
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/playwright
RUN test -n "$PLAYWRIGHT_VERSION" \
    && npx --yes "playwright@$PLAYWRIGHT_VERSION" install --with-deps --only-shell chromium \
    && npm cache clean --force
WORKDIR /work/source
