#!/bin/sh
# Prints the transcript of every case file, one block per case.
# Usage (inside the build container): sh tests/cases/record.sh
set -e
bin=./target/release/minivectordb
for f in tests/cases/*.sql; do
    echo "=== $f"
    dir=$(mktemp -d)
    awk '/^-- expect/{exit} /^--/{next} NF' "$f" | "$bin" --data "$dir" 2>/dev/null | sed '/^READY$/d'
    rm -rf "$dir"
done