#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)
tests="$repo/unittest/oblib/vector/cuvs_bridge_contract"
bridge="$repo/tools/hipvs_bridge"
output=$(mktemp -d)
trap 'rm -rf -- "$output"' EXIT
cd "$output"
cc -std=c11 -D__HIP_PLATFORM_AMD__ -Wall -Wextra -Werror -I"$tests/include" -I"$bridge" -c "$bridge/seekdb_cuvs_bridge.c" -o bridge-c.o
c++ -x c++ -std=c++17 -D__HIP_PLATFORM_AMD__ -Wall -Wextra -Werror -Wno-missing-field-initializers -I"$tests/include" -I"$bridge" -c "$bridge/seekdb_cuvs_bridge.c" -o bridge-cpp.o
for algorithm in 0 1 2 3 4; do
  cc -std=c11 -D__HIP_PLATFORM_AMD__ -DSK_CAGRA_BUILD_ALGO="$algorithm" -Wall -Wextra -Werror -I"$tests/include" -I"$bridge" -fsyntax-only "$bridge/seekdb_cuvs_bridge.c"
  c++ -x c++ -std=c++17 -D__HIP_PLATFORM_AMD__ -DSK_CAGRA_BUILD_ALGO="$algorithm" -Wall -Wextra -Werror -Wno-missing-field-initializers -I"$tests/include" -I"$bridge" -fsyntax-only "$bridge/seekdb_cuvs_bridge.c"
done
echo 'PASS: all five construction paths compile as C11 and C++17 (host API headers)'
c++ -std=c++17 -O1 -g -pthread -fsanitize=address,undefined -fno-omit-frame-pointer -D__HIP_PLATFORM_AMD__ -I"$tests/include" -I"$bridge" "$tests/test_contract.cpp" "$bridge/seekdb_cuvs_bridge.c" -o contract
ASAN_OPTIONS=detect_leaks=1 ./contract
nm bridge-c.o | grep -E ' T seekdb_cuvs_(build|search|serialize|deserialize|free|cagra_knn)$'
