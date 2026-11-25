$ErrorActionPreference = 'Stop'
$taskRoot = Split-Path -Parent $PSScriptRoot
$taskCargoDirectory = Join-Path $taskRoot '.tools\cargo\bin'
if (Test-Path -LiteralPath (Join-Path $taskCargoDirectory 'cargo.exe')) {
    $env:RUSTUP_HOME = Join-Path $taskRoot '.tools\rustup'
    $env:CARGO_HOME = Join-Path $taskRoot '.tools\cargo'
    $env:PATH = $taskCargoDirectory + ';' + $env:PATH
}
$taskCompilerDirectory = Join-Path $taskRoot '.tools\w64devkit\bin'
if (Test-Path -LiteralPath (Join-Path $taskCompilerDirectory 'gcc.exe')) {
    $env:PATH = $taskCompilerDirectory + ';' + $env:PATH
    # w64devkit combines its unwind implementation into libgcc.a.
    $taskLibGcc = & (Join-Path $taskCompilerDirectory 'gcc.exe') -print-libgcc-file-name
    $taskLibEh = Join-Path (Split-Path $taskLibGcc) 'libgcc_eh.a'
    if (-not (Test-Path -LiteralPath $taskLibEh)) { Copy-Item -LiteralPath $taskLibGcc -Destination $taskLibEh }
}
