param(
    [string]$DataDir = '',
    [string]$Bind = '127.0.0.1:8899',
    [switch]$EnableRegistration,
    [switch]$Release
)
. (Join-Path $PSScriptRoot 'Environment.ps1')
if (-not $DataDir) { $DataDir = Join-Path $taskRoot '.runtime\server' }
$taskProfile = if ($Release) { 'release' } else { 'debug' }
$taskBinary = Join-Path $taskRoot "src\server\target\$taskProfile\snapdock-server.exe"
if (-not (Test-Path -LiteralPath $taskBinary)) {
    $taskBuildArgs = @('build', '--manifest-path', (Join-Path $taskRoot 'src\server\Cargo.toml'))
    if ($Release) { $taskBuildArgs += '--release' }
    & cargo @taskBuildArgs
    if ($LASTEXITCODE -ne 0) { throw 'Rust backend build failed' }
}
& $taskBinary --data-dir $DataDir init
if ($LASTEXITCODE -ne 0) { throw 'Server initialization failed' }
if ($EnableRegistration) { & $taskBinary --data-dir $DataDir settings registration_enabled true }
Write-Host "Client server URL: https://$Bind"
Write-Host "Client certificate to import: $(Join-Path $DataDir 'server.crt')"
& $taskBinary --data-dir $DataDir serve --bind $Bind
