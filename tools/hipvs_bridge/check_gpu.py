#!/usr/bin/env python3
"""Fail early when a ROCm device allocates memory but cannot execute a kernel.

Development diagnostic only; this is not part of the official timed workload.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

SOURCE = r"""
#include <hip/hip_runtime.h>
#include <cstdio>
__global__ void probe(int *value) { value[0] = 42; }
int main() {
  int *device = nullptr;
  hipError_t status = hipMalloc((void **)&device, sizeof(int));
  std::printf("allocate=%d\n", int(status)); std::fflush(stdout);
  if (status != hipSuccess) return 1;
  hipLaunchKernelGGL(probe, dim3(1), dim3(64), 0, 0, device);
  status = hipGetLastError();
  std::printf("launch=%d\n", int(status)); std::fflush(stdout);
  if (status != hipSuccess) { hipFree(device); return 2; }
  status = hipDeviceSynchronize();
  std::printf("synchronize=%d\n", int(status)); std::fflush(stdout);
  if (status != hipSuccess) { hipFree(device); return 3; }
  int value = 0;
  status = hipMemcpy(&value, device, sizeof(value), hipMemcpyDeviceToHost);
  std::printf("copy=%d value=%d\n", int(status), value); std::fflush(stdout);
  const hipError_t release_status = hipFree(device);
  return status == hipSuccess && release_status == hipSuccess && value == 42 ? 0 : 4;
}
"""

def decoded(value):
    return value.decode(errors="replace") if isinstance(value, bytes) else (value or "")

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=Path("gpu-health.json"))
    args = parser.parse_args()
    compiler = os.environ.get("HIPCC", "/opt/rocm/bin/hipcc")
    result = {"passed": False, "stage": "compile", "compiler": compiler,
              "performance_measured": False}
    try:
        with tempfile.TemporaryDirectory(prefix="seekdb-gpu-health-") as folder:
            source = Path(folder) / "probe.hip"
            binary = Path(folder) / "probe"
            source.write_text(SOURCE, encoding="utf8")
            built = subprocess.run([compiler, str(source), "-o", str(binary)],
                                   capture_output=True, text=True, timeout=90)
            result["compile_exit"] = built.returncode
            result["compile_log"] = built.stdout + built.stderr
            if built.returncode == 0:
                result["stage"] = "execute"
                ran = subprocess.run([str(binary)], capture_output=True, text=True, timeout=20)
                result.update(execute_exit=ran.returncode, execute_log=ran.stdout + ran.stderr,
                              passed=ran.returncode == 0)
    except subprocess.TimeoutExpired as error:
        result.update(timed_out=True, partial_stdout=decoded(error.stdout),
                      partial_stderr=decoded(error.stderr))
    except OSError as error:
        result["error"] = str(error)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2), encoding="utf8")
    print(json.dumps(result, indent=2))
    return 0 if result["passed"] else 1

if __name__ == "__main__":
    raise SystemExit(main())
