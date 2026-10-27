# Sourced by the deploy scripts: loads deploy/config and sets HOST, ROOT,
# INSTANCE (the deployment config's path) and NAME (its `name`).
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
config="$ROOT/deploy/config"
[ -f "$config" ] || { echo "missing $config; copy deploy/config.example and fill it in" >&2; exit 1; }
. "$config"
: "${DOCS_PRIVATE_IP:?set DOCS_PRIVATE_IP in deploy/config}"
# The server has no public address and the domain points at the edge, so ssh
# goes to the private IP (through the edge: a ProxyJump in ~/.ssh/config).
HOST=${DOCS_HOST:-root@$DOCS_PRIVATE_IP}
INSTANCE="$ROOT/${DOCS_INSTANCE:?set DOCS_INSTANCE in deploy/config}"
NAME=$(sed -n 's/^name *= *"\(.*\)".*/\1/p' "$INSTANCE" | head -1)
NAME=${NAME:-docs}
