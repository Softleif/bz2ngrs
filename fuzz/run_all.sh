#!/usr/bin/env bash
# Usage: ./fuzz/run_all.sh [seconds-per-target]   (default 60)

set -euo pipefail
# Lets the pinned stable toolchain accept cargo-fuzz's `-Z` flags.
export RUSTC_BOOTSTRAP=1

DURATION="${1:-60}"
FUZZ_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$(dirname "$FUZZ_DIR")"

# A decoder that allocates from a header field is an OOM finding.
RSS_LIMIT=4096
ARCH="$(rustc -vV | awk '/^host/ {print $2}')"
failed=()

for target in $(cargo fuzz list); do
    corpus="$FUZZ_DIR/corpus/$target"
    mkdir -p "$corpus"
    # First dir is the read-write corpus; seeds are read-only and stay untouched.
    dirs=("$corpus")
    [ -d "$FUZZ_DIR/seeds/$target" ] && dirs+=("$FUZZ_DIR/seeds/$target")
    extra=()
    [ "$target" = decompress_differential ] && extra+=("-dict=$FUZZ_DIR/bzip2.dict")

    echo "== $target (${DURATION}s)"
    if ! cargo fuzz run --target "$ARCH" "$target" -- \
        -max_total_time="$DURATION" -rss_limit_mb="$RSS_LIMIT" -max_len=262144 \
        ${extra[@]+"${extra[@]}"} "${dirs[@]}"; then
        failed+=("$target")
    fi
done

if [ "${#failed[@]}" -gt 0 ]; then
    echo "FAILED: ${failed[*]}"
    echo "Reproduce: RUSTC_BOOTSTRAP=1 cargo fuzz run <target> fuzz/artifacts/<target>/<crash-file>"
    exit 1
fi
echo "All targets clean."
