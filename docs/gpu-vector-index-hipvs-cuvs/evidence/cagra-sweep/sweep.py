#!/usr/bin/env python3
"""Sweep CAGRA construction parameters against the real hipVS build path.

Compiles tools/hipvs_bridge/seekdb_cuvs_bridge.c once per configuration with
-DSK_CAGRA_* overrides, links the dispatch shim from the query-worker evidence
directory, and times build plus first query with benchmark.py.

Run on the Radeon Cloud instance from the repository root:

    AMD_TEST_ROWS=250000 python3 docs/gpu-vector-index-hipvs-cuvs/evidence/cagra-sweep/sweep.py

NN-descent configurations are written as graph_degree:intermediate_degree:niter.
IVF-PQ configurations are written as
ivfpq:graph_degree:intermediate_degree:n_lists:n_probes[:pq_bits]. Pass them as
arguments to override the default list.
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
    "ivfpq:160:192:512:20:8",
    "ivfpq:160:192:1024:32:8",
    "ivfpq:160:192:2048:64:8",
]


def run(cmd: list[str], env: dict[str, str], what: str) -> None:
    proc = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=900)
    if proc.returncode:
        raise SystemExit(f"{what} failed:\n{proc.stdout[-2000:]}\n{proc.stderr[-4000:]}")


def parse_config(spec: str) -> tuple[str, list[str]]:
    parts = spec.split(":")
    if parts[0] == "bf":
        if len(parts) != 2:
            raise SystemExit(f"bad brute-force config {spec!r}; want bf:graph_degree")
        degree = int(parts[1])
        tag = f"bf{degree}"
        flags = ["-DSK_CAGRA_BUILD_ALGO=3", f"-DSK_CAGRA_GRAPH_DEGREE={degree}"]
        return tag, flags
    if parts[0] == "ivfpq":
        if len(parts) not in (5, 6, 7):
            raise SystemExit(
                f"bad IVF-PQ config {spec!r}; want "
                "ivfpq:degree:intermediate:lists:probes[:pq_bits[:refine]]"
            )
        degree, intermediate, lists, probes = (int(part) for part in parts[1:5])
        pq_bits = int(parts[5]) if len(parts) >= 6 else 8
        refine = parts[6] if len(parts) == 7 else "1"
        tag = f"pq{lists}p{probes}b{pq_bits}r{refine.replace('.', '')}d{degree}i{intermediate}"
        flags = [
            "-DSK_CAGRA_BUILD_ALGO=1",
            f"-DSK_CAGRA_GRAPH_DEGREE={degree}",
            f"-DSK_CAGRA_INTERMEDIATE_DEGREE={intermediate}",
            f"-DSK_CAGRA_IVFPQ_LISTS={lists}",
            f"-DSK_CAGRA_IVFPQ_PROBES={probes}",
            f"-DSK_CAGRA_IVFPQ_PQ_BITS={pq_bits}",
            f"-DSK_CAGRA_IVFPQ_REFINE={refine}",
        ]
        return tag, flags
    if len(parts) != 3:
        raise SystemExit(f"bad NN-descent config {spec!r}; want degree:intermediate:niter")
    degree, intermediate, niter = (int(part) for part in parts)
    tag = f"d{degree}i{intermediate}n{niter}"
    flags = [
        "-DSK_CAGRA_BUILD_ALGO=2",
        f"-DSK_CAGRA_GRAPH_DEGREE={degree}",
        f"-DSK_CAGRA_INTERMEDIATE_DEGREE={intermediate}",
        f"-DSK_CAGRA_NITER={niter}",
    ]
    return tag, flags


def build_variant(tag: str, flags: list[str], env: dict[str, str]) -> None:
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
            *flags,
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
        tag, flags = parse_config(spec)
        build_variant(tag, flags, env)
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
