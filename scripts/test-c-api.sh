#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"
toolchain=${SKRIN_TEST_TOOLCHAIN:-1.89.0}
profile=debug
profile_args=()
if [[ ${1:-} == --release ]]; then
    profile=release
    profile_args=(--release)
    shift
fi
if [[ $# != 0 ]]; then
    echo "usage: scripts/test-c-api.sh [--release]" >&2
    exit 2
fi
host=$(rustc "+$toolchain" -vV | sed -n 's/^host: //p')
if [[ -n ${CARGO_BUILD_TARGET:-} && $CARGO_BUILD_TARGET != "$host" ]]; then
    echo "This script tests native C linkage; CARGO_BUILD_TARGET must match $host" >&2
    exit 2
fi
target_root=${CARGO_TARGET_DIR:-"$repo_root/target"}
if [[ $target_root != /* ]]; then target_root="$repo_root/$target_root"; fi
artifact_root="$target_root"
if [[ -n ${CARGO_BUILD_TARGET:-} ]]; then artifact_root="$artifact_root/$CARGO_BUILD_TARGET"; fi
scratch=$(mktemp -d "${TMPDIR:-/tmp}/skrin-c-api.XXXXXX")
trap 'rm -rf -- "$scratch"' EXIT
build_log="$scratch/build.log"
# Ask the actual Rust staticlib build for its native link dependencies. Cargo
# may replay no compiler notes on a cached build; retain the exact flags next
# to that target directory, qualified by compiler version and target.
compiler=$(rustc "+$toolchain" --version | tr ' /()' '_____')
flags_file="$artifact_root/$profile/skrin-native-libs-$compiler-$host.txt"
if ! CARGO_TERM_COLOR=never cargo "+$toolchain" rustc -p skrin-ffi --locked \
    "${profile_args[@]}" -- --print=native-static-libs >"$build_log" 2>&1; then
    cat "$build_log" >&2
    exit 1
fi
cat "$build_log"
native_flags=$(sed -n 's/^note: native-static-libs: //p' "$build_log" | tail -1)
if [[ -n $native_flags ]]; then
    printf '%s\n' "$native_flags" >"$flags_file"
elif [[ -f $flags_file ]]; then
    native_flags=$(cat "$flags_file")
else
    echo "Native link flags were not emitted or cached; clean skrin-ffi for this profile and rerun" >&2
    exit 1
fi
read -r -a native_libs <<< "$native_flags"
library="$artifact_root/$profile/libskrin_ffi.a"
test -f "$library"
c_compiler=${CC:-cc}
cxx_compiler=${CXX:-c++}
sanitizer_flags=()
if [[ ${SKRIN_C_SANITIZE:-0} == 1 ]]; then
    sanitizer_flags=(-fsanitize=address,undefined -fno-omit-frame-pointer)
fi
include="$repo_root/crates/skrin-ffi/include"
"$c_compiler" -std=c11 -O2 -Wall -Wextra -Werror -pedantic "${sanitizer_flags[@]}" \
    -I "$include" crates/skrin-ffi/examples/accounts.c "$library" "${native_libs[@]}" -o "$scratch/accounts"
"$c_compiler" -std=c11 -O2 -Wall -Wextra -Werror -pedantic "${sanitizer_flags[@]}" \
    -I "$include" crates/skrin-ffi/tests/native.c "$library" "${native_libs[@]}" -o "$scratch/native"
"$c_compiler" -std=c11 -O2 -Wall -Wextra -Werror -pedantic "${sanitizer_flags[@]}" \
    -I "$include" crates/skrin-ffi/benches/baseline.c "$library" "${native_libs[@]}" -o "$scratch/bench"
"$cxx_compiler" -std=c++11 -Wall -Wextra -Werror -pedantic "${sanitizer_flags[@]}" \
    -I "$include" crates/skrin-ffi/tests/header.cpp "$library" "${native_libs[@]}" -o "$scratch/header-cpp"
"$scratch/header-cpp"
"$scratch/accounts"
"$scratch/accounts" "$scratch/accounts-db" "$scratch/accounts-backup"
"$scratch/native" "$scratch"
"$scratch/bench"
"$scratch/bench" "$scratch/bench-db"
