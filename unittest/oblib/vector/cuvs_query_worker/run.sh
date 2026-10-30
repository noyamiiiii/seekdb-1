#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)
source="$repo/unittest/oblib/vector/cuvs_query_worker/test_worker.cpp"
output=$(mktemp -d)
trap 'rm -rf -- "$output"' EXIT
flags=(-std=c++17 -pthread -Wall -Wextra -Werror -Wno-misleading-indentation -I"$repo/src/oblib/lib/vector" -Wl,--wrap=pthread_create -Wl,--wrap=pthread_join)
g++ "${flags[@]}" -O1 -g -fsanitize=address,undefined -fno-omit-frame-pointer "$source" -o "$output/check"
ASAN_OPTIONS=detect_leaks=1 "$output/check"
g++ "${flags[@]}" -O2 "$source" -o "$output/bench"
"$output/bench" benchmark
