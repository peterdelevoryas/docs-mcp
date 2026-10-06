# Sourced by the deploy scripts: loads deploy/config and sets HOST, ROOT,
# INSTANCE (the deployment config's path) and NAME (its `name`).
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
config="$ROOT/deploy/config"
[ -f "$config" ] || { echo "missing $config; copy deploy/config.example and fill it in" >&2; exit 1; }
. "$config"
HOST=${DOCS_HOST:-root@$DOCS_DOMAIN}
INSTANCE="$ROOT/${DOCS_INSTANCE:?set DOCS_INSTANCE in deploy/config}"
NAME=$(sed -n 's/^name *= *"\(.*\)".*/\1/p' "$INSTANCE" | head -1)
NAME=${NAME:-docs}
