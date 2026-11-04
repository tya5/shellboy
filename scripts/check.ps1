$ErrorActionPreference = 'Stop'
Push-Location (Join-Path $PSScriptRoot '..')
try {
    & python scripts/check-test-layout.py
    if ($LASTEXITCODE -ne 0) { throw 'integration layout check failed' }
    & python -m unittest discover -s scripts/tests
    if ($LASTEXITCODE -ne 0) { throw 'timing runner checks failed' }
    & cargo fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { throw 'cargo fmt failed' }
    & cargo clippy --locked --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw 'cargo clippy failed' }
    & cargo test --locked --all-targets
    if ($LASTEXITCODE -ne 0) { throw 'cargo test failed' }
    & cargo test --locked --features test-db-observation --test integration db_observation_feature::integration_worker_samples_fast_statements -- --exact
    if ($LASTEXITCODE -ne 0) { throw 'DB observation integration test failed' }
} finally {
    Pop-Location
}
