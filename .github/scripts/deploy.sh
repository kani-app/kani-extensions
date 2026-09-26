#!/usr/bin/env bash
# Publishes built extensions into the live extension repository.
#
# Pulls the live repo, verifies it, signs each pending artifact into it, verifies it again,
# and syncs it back. The pending list is re-checked against the live index, and a version
# that already exists is never overwritten.
#
# Required env:
#   KANI_CLI          kani-cli binary
#   ARTIFACT_DIR      build output: pending.json plus one <id>.wasm or <id>.yaml per extension
#   WORK_DIR          scratch directory for the pulled repo
#   AUTHOR_KEY_FILE, MAINTAINER_KEY_FILE, SSH_KEY_FILE, KNOWN_HOSTS_FILE
#   REPO_HOST         ssh destination (user@host)
#   REPO_REMOTE_DIR   repo root on the server
# Optional env:
#   REPO_PORT         ssh port (default 22)
#   DRY_RUN=1         sign locally and show what rsync would change, without writing remotely

set -euo pipefail

: "${KANI_CLI:?}" "${ARTIFACT_DIR:?}" "${WORK_DIR:?}"
: "${AUTHOR_KEY_FILE:?}" "${MAINTAINER_KEY_FILE:?}" "${SSH_KEY_FILE:?}" "${KNOWN_HOSTS_FILE:?}"
: "${REPO_HOST:?}" "${REPO_REMOTE_DIR:?}"

# Actions masks only exact secret values; ssh messages print the bare host and path.
if [ -n "${GITHUB_ACTIONS:-}" ]; then
  echo "::add-mask::${REPO_HOST#*@}"
  echo "::add-mask::$REPO_REMOTE_DIR"
fi

scripts="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$WORK_DIR/repo"
# The maintainer key is pinned here rather than read from the pulled index, so an index
# altered on the server is refused instead of being re-signed.
maintainer_pub="$scripts/../maintainer.pub"
# The pinned key uses a stable alias, so changing the address or port does not
# require changing the known_hosts entry when the server's host key is unchanged.
ssh_cmd="ssh -p ${REPO_PORT:-22} -i $SSH_KEY_FILE -o IdentitiesOnly=yes -o BatchMode=yes \
  -o StrictHostKeyChecking=yes -o HostKeyAlias=kani-extension-repo \
  -o UserKnownHostsFile=$KNOWN_HOSTS_FILE"

echo "==> Pulling the live repository"
mkdir -p "$repo"
rsync -a --delete -e "$ssh_cmd" "$REPO_HOST:$REPO_REMOTE_DIR/" "$repo/"
"$KANI_CLI" repo verify --repo-dir "$repo" --repo-key "$maintainer_pub"

python3 "$scripts/extensions.py" pending --index "$repo/index.json" > "$WORK_DIR/live-pending.json"
mapfile -t todo < <(python3 - "$ARTIFACT_DIR/pending.json" "$WORK_DIR/live-pending.json" <<'EOF'
import json, sys
built = {e["id"]: e for e in json.load(open(sys.argv[1]))}
for e in json.load(open(sys.argv[2])):
    b = built.get(e["id"])
    if b and b["version"] == e["version"]:
        print("\t".join([e["id"], e["name"], e["version"], e["format"]]))
EOF
)

if [ "${#todo[@]}" -eq 0 ]; then
  echo "==> Nothing newer than the live index; the repository is unchanged"
  exit 0
fi

for line in "${todo[@]}"; do
  IFS=$'\t' read -r id name version format <<<"$line"
  if [ -e "$repo/extensions/$id/$version" ]; then
    echo "error: $id $version already exists in the repository; bump the version" >&2
    exit 1
  fi
  echo "==> Publishing $id $version ($format)"
  args=("$ARTIFACT_DIR/$id.$format" --sign-key "$AUTHOR_KEY_FILE" --repo-dir "$repo"
        --repo-sign-key "$MAINTAINER_KEY_FILE")
  if [ "$format" = wasm ]; then
    args+=(--ext-id "$id" --ext-name "$name" --ext-version "$version")
  fi
  "$KANI_CLI" publish "${args[@]}"
done
"$KANI_CLI" repo verify --repo-dir "$repo" --repo-key "$maintainer_pub"

# --inplace and --omit-dir-times: the repo root is root-owned, so rsync can neither create
# temp files beside the targets nor set directory times there. No --delete: old versions stay.
rsync_flags=(-rlc --itemize-changes --inplace --omit-dir-times --exclude .write_test)
if [ "${DRY_RUN:-}" = 1 ]; then
  echo "==> Dry run: changes rsync would make"
  rsync "${rsync_flags[@]}" -n -e "$ssh_cmd" "$repo/" "$REPO_HOST:$REPO_REMOTE_DIR/"
  exit 0
fi

echo "==> Syncing"
rsync "${rsync_flags[@]}" -e "$ssh_cmd" "$repo/" "$REPO_HOST:$REPO_REMOTE_DIR/"

echo "==> Confirming the remote index matches"
if ! $ssh_cmd "$REPO_HOST" "cat '$REPO_REMOTE_DIR/index.json'" | cmp -s - "$repo/index.json"; then
  echo "error: the remote index.json differs from the one just signed" >&2
  exit 1
fi
echo "    remote index.json matches"
