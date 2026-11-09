# Developer helper: build/test/run the crate inside the pinned Rust container.
#
# The evaluator builds and runs everything on Linux x86_64, so the local loop
# uses the same image the Dockerfile will: no Windows toolchain involved.
#
#   powershell -File tools/dev.ps1 build    # cargo build --release
#   powershell -File tools/dev.ps1 test     # cargo test --release
#   powershell -File tools/dev.ps1 fmt      # cargo fmt --check
#   powershell -File tools/dev.ps1 clippy   # cargo clippy, warnings fatal
#   powershell -File tools/dev.ps1 shell    # interactive shell in the container
#   powershell -File tools/dev.ps1 sql "SELECT 1;"   # one shot through the binary
param(
    [Parameter(Position = 0)][string]$Action = "test",
    [Parameter(Position = 1)][string]$Sql = ""
)

$ErrorActionPreference = "Stop"
$src = Split-Path -Parent $PSScriptRoot
$image = "rust:1.88-slim-bookworm"

function Invoke-Cargo {
    param([string]$Command)
    # cargo writes progress to stderr; without relaxing EAP, PowerShell turns
    # that into a terminating NativeCommandError even when the build succeeds.
    $prev = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    docker run --rm -v "${src}:/src" -w /src $image sh -c $Command 2>&1 | Out-Host
    $code = $LASTEXITCODE
    $ErrorActionPreference = $prev
    if ($code -ne 0) { exit $code }
}

# rustfmt/clippy are not preinstalled in the slim image.
$ensureRustfmt = "rustup component list --installed | grep -q rustfmt || rustup component add rustfmt >/dev/null"
$ensureClippy = "rustup component list --installed | grep -q clippy || rustup component add clippy >/dev/null"

switch ($Action) {
    "build" { Invoke-Cargo "cargo build --release" }
    "test" { Invoke-Cargo "cargo test --release" }
    # `sh -c`, not `-lc`: the login profile in the slim image hides cargo.
    "fmt" { Invoke-Cargo "$ensureRustfmt && cargo fmt --check" }
    "clippy" { Invoke-Cargo "$ensureClippy && cargo clippy --release --all-targets -- -D warnings" }
    "sql" {
        Invoke-Cargo "cargo build --release >/dev/null 2>&1 && printf '%s\n' '$Sql' | ./target/release/minivectordb --data /tmp/mvdb"
    }
    "shell" { docker run --rm -it -v "${src}:/src" -w /src $image sh }
    default { Write-Error "unknown action: $Action (build|test|fmt|clippy|sql|shell)" }
}