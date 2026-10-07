#!/usr/bin/env bash
# Real ENOSPC in a private, bounded tmpfs; no caller filesystem is filled.
set -euo pipefail

namespace=(unshare --user --map-root-user --mount --fork --propagation private)
build_args=(test -p skrin --test storage_full --no-run --locked --message-format=json)
while [[ $# != 0 ]]; do
  case $1 in
    --privileged-namespace) namespace=(sudo -n unshare --mount --fork --propagation private) ;;
    --release) build_args+=(--release) ;;
    *) echo 'usage: scripts/test-storage-full.sh [--release] [--privileged-namespace]' >&2; exit 2 ;;
  esac
  shift
done
cd "$(dirname "${BASH_SOURCE[0]}")/.."
toolchain=${SKRIN_TEST_TOOLCHAIN:-1.89.0}
executable=$(cargo "+$toolchain" "${build_args[@]}" | python3 -c '
import json,sys
for line in sys.stdin:
 message=json.loads(line)
 if message.get("target",{}).get("name")=="storage_full" and message.get("executable"):
  print(message["executable"])
')
[[ -n $executable ]] || { echo 'missing storage-full test executable' >&2; exit 1; }
mount_root=$(mktemp -d "${TMPDIR:-/tmp}/skrin-storage-full.XXXXXX")
trap 'rmdir -- "$mount_root"' EXIT
"${namespace[@]}" sh -eu -c '
  mount -t tmpfs -o size=4m,nr_inodes=128,mode=700 tmpfs "$1"
  SKRIN_STORAGE_FULL_ROOT="$1" exec "$2" --ignored --exact real_storage_full_preserves_preparation_and_commit_outcomes --nocapture
' sh "$mount_root" "$executable"
