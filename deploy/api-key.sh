#!/bin/bash
# Install the Anthropic API key that enables ask_docs and summarize, from
# the macOS keychain (service "<name>-anthropic-api-key"), and restart.
#
# Store it first: security add-generic-password -s <name>-anthropic-api-key -a docs-mcp -w
# Usage: deploy/api-key.sh
set -euo pipefail
. "$(dirname "$0")/lib.sh"
key=$(security find-generic-password -s "$NAME-anthropic-api-key" -a docs-mcp -w)
printf 'ANTHROPIC_API_KEY=%s\n' "$key" | ssh "$HOST" \
    'umask 077; cat > /etc/docs-mcp/anthropic.env && systemctl restart docs-mcp'
echo "installed; ask_docs and summarize are enabled" >&2
