FROM cgr.dev/chainguard/node:latest-dev

USER root
RUN apk add --no-cache git bash aws-cli jq tmux
RUN npm install -g @anthropic-ai/claude-code

COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

USER node
WORKDIR /workspace
ENTRYPOINT ["/entrypoint.sh"]
