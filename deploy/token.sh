#!/bin/bash
# Mint a client token on the server and store it in the macOS keychain
# (service "<name>-mcp-token", account = source). The server keeps only the
# token's hash. Re-running for the same source adds a second token; remove
# the old line from /etc/docs-mcp/tokens to revoke it.
#
# Usage: deploy/token.sh <source>
# Read it back: security find-generic-password -s <name>-mcp-token -a <source> -w
set -euo pipefail
. "$(dirname "$0")/lib.sh"
[ $# -eq 1 ] || { sed -n 's/^# \{0,1\}//; 2,9p' "$0" >&2; exit 2; }
source=$1

token=$(ssh "$HOST" bash -s -- "$source" <<'REMOTE'
set -euo pipefail
err=$(mktemp); trap 'rm -f $err' EXIT
line=$(docs-mcp token "$1" 2>"$err") || { cat "$err" >&2; exit 1; }
echo "$line" >> /etc/docs-mcp/tokens
systemctl reload docs-mcp
sed -n 2p "$err"
REMOTE
)
security add-generic-password -U -s "$NAME-mcp-token" -a "$source" -w "$token"
echo "minted token for $source; stored in keychain ($NAME-mcp-token / $source)" >&2
