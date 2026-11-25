. (Join-Path $PSScriptRoot 'Environment.ps1')
Push-Location $taskRoot
try {
    & cargo test -p snapdock
    if ($LASTEXITCODE -ne 0) { throw 'Desktop client Rust tests failed' }
    & cargo build -p snapdock
    if ($LASTEXITCODE -ne 0) { throw 'Desktop client Rust build failed' }
    & cargo test --locked --manifest-path src/server/Cargo.toml
    if ($LASTEXITCODE -ne 0) { throw 'Sync server Rust tests failed' }
    & cargo build --locked --manifest-path src/server/Cargo.toml
    if ($LASTEXITCODE -ne 0) { throw 'Sync server Rust build failed' }
} finally { Pop-Location }
