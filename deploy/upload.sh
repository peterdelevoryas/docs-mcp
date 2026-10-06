#!/bin/bash
# Copy a document to the server and ingest it (replacing any document with
# the same id). The file stays on the server, readable only by the
# docs-mcp user; it is never committed.
#
# Usage: deploy/upload.sh <file> <id> <title> [description]
# e.g.   deploy/upload.sh ~/Downloads/manual.pdf widget-2.0 'Widget Manual 2.0'
set -euo pipefail
. "$(dirname "$0")/lib.sh"
[ $# -ge 3 ] && [ $# -le 4 ] || { sed -n 's/^# \{0,1\}//; 2,8p' "$0" >&2; exit 2; }
file=$1 id=$2
ext=${file##*.}
name="$id.$ext"
ssh "$HOST" 'install -d -o docs-mcp -g docs-mcp -m 0700 /var/lib/docs-mcp /var/lib/docs-mcp/docs'
scp -q "$file" "$HOST:/var/lib/docs-mcp/docs/$name"
ssh "$HOST" bash -s -- "$name" "${@:2}" <<'REMOTE'
set -euo pipefail
name=$1; shift
chown docs-mcp:docs-mcp "/var/lib/docs-mcp/docs/$name" && chmod 0600 "/var/lib/docs-mcp/docs/$name"
runuser -u docs-mcp -- env DOCS_DB=/var/lib/docs-mcp/docs.db docs-mcp ingest "/var/lib/docs-mcp/docs/$name" "$@"
REMOTE
