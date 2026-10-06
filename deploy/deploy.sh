#!/bin/bash
# Build on the VM and (re)install, with the deployment config named in
# deploy/config. Usage: deploy/deploy.sh [host]
set -euo pipefail
. "$(dirname "$0")/lib.sh"
HOST=${1:-$HOST}
cd "$ROOT"
ssh "$HOST" mkdir -p /root/src/docs-mcp
rsync -az --delete --exclude target --exclude .git --exclude '*.db*' --exclude '*.pdf' \
    --exclude /docs --exclude /tokens --exclude /deploy/config ./ "$HOST:/root/src/docs-mcp/"
scp -q "$INSTANCE" "$HOST:/root/src/docs-mcp/instance.toml"
ssh "$HOST" bash -se -- "$DOCS_DOMAIN" <<'REMOTE'
set -euo pipefail
DOMAIN=$1
cd /root/src/docs-mcp
~/.cargo/bin/cargo build --release --locked
install -m 0755 target/release/docs-mcp /usr/local/bin/docs-mcp
install -m 0644 instance.toml /etc/docs-mcp/config.toml
# The domain lives in the untracked deploy/config, not in the repo.
sed "s/@DOMAIN@/$DOMAIN/g" deploy/docs-mcp.service > /etc/systemd/system/docs-mcp.service
sed "s/@DOMAIN@/$DOMAIN/g" deploy/Caddyfile > /etc/caddy/Caddyfile
touch /etc/docs-mcp/tokens && chown root:docs-mcp /etc/docs-mcp/tokens && chmod 0640 /etc/docs-mcp/tokens
systemctl daemon-reload
systemctl enable --now docs-mcp
systemctl restart docs-mcp
systemctl reload caddy
systemctl --no-pager --lines=5 status docs-mcp
REMOTE
