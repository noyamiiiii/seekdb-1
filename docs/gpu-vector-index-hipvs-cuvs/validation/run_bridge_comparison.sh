#!/usr/bin/env bash
set -euo pipefail
repo=$(git rev-parse --show-toplevel)
output=${1:-/tmp/seekdb-cagra-validation}
frozen=08c76418a8e1fef3c1aae83010658ff56a4faa57
export AMD_BRIDGE_ROOT="$output"
export OPENBLAS_NUM_THREADS=4 OMP_NUM_THREADS=4
export LD_LIBRARY_PATH="/opt/hipvs/lib:/opt/rocm/lib:${LD_LIBRARY_PATH:-}"
mkdir -p "$output"
git -C "$repo" show "$frozen:tools/hipvs_bridge/seekdb_cuvs_bridge.c" > "$output/baseline.c"
cp "$repo/tools/hipvs_bridge/seekdb_cuvs_bridge.c" "$output/candidate.c"
cp "$output/candidate.c" "$output/candidate-final.c"
cp "$repo/tools/hipvs_bridge/seekdb_cuvs_bridge.h" "$output/seekdb_cuvs_bridge.h"
for version in baseline candidate; do
  /opt/rocm/bin/hipcc -shared -fPIC -O2 -pthread -D__HIP_PLATFORM_AMD__ \
    -I/opt/hipvs/include -I/opt/rocm/include "$output/$version.c" \
    -L/opt/hipvs/lib -lcuvs_c -Wl,-rpath,/opt/hipvs/lib:/opt/rocm/lib \
    -o "$output/lib$version.so"
done
python3 "$repo/docs/gpu-vector-index-hipvs-cuvs/validation/validate_cagra_bridge.py"
python3 "$repo/docs/gpu-vector-index-hipvs-cuvs/validation/bench_cagra_bridge.py"
echo "Bridge-only measurements and validation are in $output; these are not official SQL build scores."
