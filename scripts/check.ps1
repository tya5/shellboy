$ErrorActionPreference = 'Stop'
Push-Location (Join-Path $PSScriptRoot '..')
try {
    & python scripts/check-test-layout.py
    if ($LASTEXITCODE -ne 0) { throw 'integration layout check failed' }
    & cargo fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { throw 'cargo fmt failed' }
    & cargo clippy --locked --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw 'cargo clippy failed' }
    & cargo test --locked --all-targets
    if ($LASTEXITCODE -ne 0) { throw 'cargo test failed' }
} finally {
    Pop-Location
}
