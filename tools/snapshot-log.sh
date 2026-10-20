#!/usr/bin/env bash
# Snapshots LOG.md and systems.yaml onto the local log-md branch, which
# holds those two files and nothing else, without touching the working
# tree or the main branch: the two are listed in .git/info/exclude and
# never go on main.  Uses a scratch index, so the branch is never checked
# out (that would pull LOG.md from under an editor).
#
# usage: tools/snapshot-log.sh [MESSAGE]

set -euo pipefail

cd "$(dirname "$0")/.."

index=.git/log-md.index
rm -f "$index"
export GIT_INDEX_FILE=$index
git read-tree --empty
git add -f LOG.md systems.yaml
tree=$(git write-tree)
unset GIT_INDEX_FILE
rm -f "$index"

parent=$(git rev-parse -q --verify refs/heads/log-md 2>/dev/null || true)
if [ -n "$parent" ] && [ "$(git rev-parse "$parent^{tree}")" = "$tree" ]; then
    echo "log-md: unchanged"
    exit 0
fi
message=${1:-"log as of $(date '+%Y-%m-%d %H:%M')"}
commit=$(echo "$message" | git commit-tree "$tree" ${parent:+-p "$parent"})
git update-ref refs/heads/log-md "$commit" ${parent:-}
echo "log-md: $commit"
