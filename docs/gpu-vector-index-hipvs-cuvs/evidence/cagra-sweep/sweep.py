#!/usr/bin/env python3
"""Sweep CAGRA construction parameters against the real hipVS build path.

Compiles tools/hipvs_bridge/seekdb_cuvs_bridge.c once per configuration with
-DSK_CAGRA_* overrides, links the dispatch shim from the query-worker evidence
directory, and times build plus first query with benchmark.py.

Run on the Radeon Cloud instance from the repository root:

    AMD_TEST_ROWS=250000 python3 docs/gpu-vector-index-hipvs-cuvs/evidence/cagra-sweep/sweep.py

Configurations are written as graph_degree:intermediate_degree:niter. Pass them
as arguments to override the default list.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
BRIDGE_DIR = ROOT / "tools" / "hipvs_bridge"
HARNESS = ROOT / "docs" / "gpu-vector-index-hipvs-cuvs" / "evidence" / "query-worker" / "gpu"
OUT = Path(__file__).resolve().parent

DEFAULT_CONFIGS = [
    "160:192:40",
    "160:192:35",
    "160:192:30",
    "176:192:40",
    "144:192:40",
    "128:192:40",
]


def run(cmd: list[str], env: dict[str, str], what: str) -> None:
    proc = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=900)
    if proc.returncode:
        raise SystemExit(f"{what} failed:\n{proc.stdout[-2000:]}\n{proc.stderr[-4000:]}")


def build_variant(tag: str, degree: int, intermediate: int, niter: int, env: dict[str, str]) -> None:
    bridge_so = OUT / f"libbridge_{tag}.so"
    # benchmark.py loads libdispatch_<tag>.so from its own directory.
    dispatch_so = HARNESS / f"libdispatch_{tag}.so"
    run(
        [
            "/opt/rocm/bin/hipcc",
            "-shared",
            "-fPIC",
            "-O2",
            "-pthread",
            "-D__HIP_PLATFORM_AMD__",
            f"-DSK_CAGRA_GRAPH_DEGREE={degree}",
            f"-DSK_CAGRA_INTERMEDIATE_DEGREE={intermediate}",
            f"-DSK_CAGRA_NITER={niter}",
            "-I/opt/hipvs/include",
            "-I/opt/rocm/include",
            f"-I{BRIDGE_DIR}",
            "-I" + str(ROOT / "src" / "oblib" / "lib" / "vector"),
            str(BRIDGE_DIR / "seekdb_cuvs_bridge.c"),
            "-L/opt/hipvs/lib",
            "-lcuvs_c",
            "-Wl,-rpath,/opt/hipvs/lib:/opt/rocm/lib",
            "-o",
            str(bridge_so),
        ],
        env,
        f"bridge compile {tag}",
    )
    run(
        [
            "g++",
            "-std=c++17",
            "-O2",
            "-shared",
            "-fPIC",
            "-pthread",
            f"-I{BRIDGE_DIR}",
            "-I" + str(ROOT / "src" / "oblib" / "lib" / "vector"),
            str(HARNESS / "dispatch.cpp"),
            "-L" + str(OUT),
            f"-lbridge_{tag}",
            "-Wl,-rpath," + str(OUT),
            "-o",
            str(dispatch_so),
        ],
        env,
        f"dispatch compile {tag}",
    )


def main() -> int:
    configs = sys.argv[1:] or DEFAULT_CONFIGS
    env = os.environ.copy()
    env["LD_LIBRARY_PATH"] = f"{OUT}:{HARNESS}:/opt/hipvs/lib:/opt/rocm/lib"
    env.setdefault("OPENBLAS_NUM_THREADS", "4")
    env.setdefault("AMD_TEST_ROWS", "250000")
    dims = [int(item) for item in env.get("AMD_SWEEP_DIMS", "128,256").split(",")]

    rows: list[dict] = []
    for spec in configs:
        degree, intermediate, niter = (int(part) for part in spec.split(":"))
        tag = f"d{degree}i{intermediate}n{niter}"
        build_variant(tag, degree, intermediate, niter, env)
        for dim in dims:
            proc = subprocess.run(
                [sys.executable, str(HARNESS / "benchmark.py"), tag, str(dim)],
                env=env,
                cwd=str(HARNESS),
                capture_output=True,
                text=True,
                timeout=1800,
            )
            log = OUT / f"{tag}-{dim}.log"
            log.write_text(proc.stdout + proc.stderr)
            if proc.returncode:
                rows.append({"config": spec, "dim": dim, "error": proc.stderr[-1500:]})
                print(f"{spec} dim={dim} FAILED, see {log}", flush=True)
                continue
            report = json.loads((HARNESS / f"{tag}-{dim}.json").read_text())
            row = {
                "config": spec,
                "dim": dim,
                "build_sec": report.get("build_first_sec"),
                "reload_sec": report.get("reload_sec"),
                "recall10": report["modes"]["10"]["recall"],
                "recall100": report["modes"]["100"]["recall"],
            }
            rows.append(row)
            print(
                f"{spec:>14} dim={dim:<4} build={row['build_sec']:.3f}s "
                f"recall@10={row['recall10']:.4f} recall@100={row['recall100']:.4f}",
                flush=True,
            )

    (OUT / "sweep-results.json").write_text(json.dumps(rows, indent=2))

    by_dim: dict[int, float] = {}
    for row in rows:
        if "build_sec" in row:
            fastest = by_dim.get(row["dim"])
            if fastest is None or row["build_sec"] < fastest:
                by_dim[row["dim"]] = row["build_sec"]
    print("\nrelative to the fastest configuration per dimension")
    for row in rows:
        if "build_sec" not in row:
            continue
        speedup = by_dim[row["dim"]] / row["build_sec"]
        print(f"{row['config']:>14} dim={row['dim']:<4} {speedup:.2f}x  recall@10={row['recall10']:.4f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
