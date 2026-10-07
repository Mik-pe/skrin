#!/usr/bin/env bash
# Real kernel OOM confined to one disposable process group, never the caller.
set -euo pipefail
launcher=(systemd-run --user)
manager=(systemctl --user)
build_args=(test -p skrin --test memory_budget --no-run --locked --message-format=json)
while [[ $# != 0 ]]; do
  case $1 in
    --privileged-manager)
      launcher=(sudo -n systemd-run --uid="$(id -u)" --gid="$(id -g)")
      manager=(sudo -n systemctl)
      ;;
    --release) build_args+=(--release) ;;
    *) echo 'usage: scripts/test-memory-budget.sh [--release] [--privileged-manager]' >&2; exit 2 ;;
  esac
  shift
done
cd "$(dirname "${BASH_SOURCE[0]}")/.."
toolchain=${SKRIN_TEST_TOOLCHAIN:-1.89.0}
executable=$(cargo "+$toolchain" "${build_args[@]}" | python3 -c '
import json,sys
for line in sys.stdin:
 message=json.loads(line)
 if message.get("target",{}).get("name")=="memory_budget" and message.get("executable"):
  print(message["executable"])
')
[[ -n $executable ]] || { echo 'missing memory-budget executable' >&2; exit 1; }
root=$(mktemp -d "${TMPDIR:-/tmp}/skrin-memory-budget.XXXXXX")
mkdir -- "$root/work"
log="$root/worker.log"
unit="skrin-memory-budget-$$-$(date +%s%N).service"
complete=false
cleanup() {
  "${manager[@]}" reset-failed "$unit" >/dev/null 2>&1 || true
  if $complete; then rm -rf -- "$root"; else echo "test evidence retained at $root" >&2; fi
}
trap cleanup EXIT
set +e
"${launcher[@]}" --wait --pipe --unit="$unit" \
  -p MemoryAccounting=yes -p MemoryMax=64M -p MemorySwapMax=0 \
  -p OOMPolicy=kill -p LimitCORE=0 -p RuntimeMaxSec=30s \
  --setenv="SKRIN_MEMORY_BUDGET_ROOT=$root/work" \
  "$executable" --ignored --exact bounded_checkpoint_worker --nocapture 2>&1 | tee "$log"
status=$?
set -e
[[ $status != 0 ]] || { echo 'worker unexpectedly survived' >&2; exit 1; }
result=$("${manager[@]}" show "$unit" --property=Result --value)
"${manager[@]}" show "$unit" --property=Result --property=ExecMainCode --property=ExecMainStatus --property=MemoryPeak
[[ $result == oom-kill ]] || { echo "expected actual kernel OOM, got $result" >&2; exit 1; }
grep -q '^checkpoint decoder allocating 128 MiB of touched scratch$' "$log"
SKRIN_MEMORY_BUDGET_ROOT="$root/work" "$executable" --ignored --exact recover_after_bounded_checkpoint_worker --nocapture
complete=true
