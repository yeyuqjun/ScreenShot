. (Join-Path $PSScriptRoot 'Environment.ps1')
Push-Location $taskRoot
try {
    & cargo run -p snapdock
} finally { Pop-Location }
