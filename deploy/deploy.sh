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
ssh "$HOST" bash -se -- "$DOCS_DOMAIN" "$DOCS_PRIVATE_IP" <<'REMOTE'
set -euo pipefail
DOMAIN=$1
PRIVATE_IP=$2
cd /root/src/docs-mcp
~/.cargo/bin/cargo build --release --locked
install -m 0755 target/release/docs-mcp /usr/local/bin/docs-mcp
install -m 0644 instance.toml /etc/docs-mcp/config.toml
# The domain and private IP live in the untracked deploy/config, not in the repo.
sed "s/@DOMAIN@/$DOMAIN/g; s/@PRIVATE_IP@/$PRIVATE_IP/g" deploy/docs-mcp.service > /etc/systemd/system/docs-mcp.service
# The edge terminates TLS for the domain and proxies here over the private
# network.
ufw allow from 10.0.0.0/16 to any port 8770 proto tcp >/dev/null
touch /etc/docs-mcp/tokens && chown root:docs-mcp /etc/docs-mcp/tokens && chmod 0640 /etc/docs-mcp/tokens
systemctl daemon-reload
systemctl enable --now docs-mcp
systemctl restart docs-mcp
systemctl --no-pager --lines=5 status docs-mcp
REMOTE
