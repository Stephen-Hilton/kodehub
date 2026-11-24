# The one small agent base image (spec §4.1): the kodehub-worker entrypoint (`agent`), the
# kodehub CLI as the agent's MCP server (read-only graph tools), and the official `claude` CLI
# (a standalone native binary; Node is only used to fetch it, in the builder stage). A role,
# instructions, tools and context are supplied per task; nothing is baked in.
FROM kodehub/kodehub:dev AS kodehub

FROM kodehub/test-core:dev AS claude
USER root
ARG NODE_VERSION=22.11.0
ARG CLAUDE_CODE_VERSION=2.1.318
RUN set -eux; \
    arch="$(dpkg --print-architecture)"; case "$arch" in amd64) n=x64;; arm64) n=arm64;; *) n="$arch";; esac; \
    apt-get update; apt-get install -y --no-install-recommends curl xz-utils ca-certificates; \
    curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-${n}.tar.xz" \
      | tar -xJ -C /usr/local --strip-components=1; \
    npm install -g --omit=dev "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}"; \
    cp "$(readlink -f /usr/local/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe)" /claude; \
    /claude --version

FROM kodehub/test-core:dev
COPY --from=claude /claude /usr/local/bin/claude
COPY --from=kodehub /usr/local/bin/kodehub /usr/local/bin/kodehub
COPY --from=kodehub /app/recordings /app/recordings
ENV KODEHUB_HOME=/work/kode
WORKDIR /work
