param([switch]$SkipServerBuild)
. (Join-Path $PSScriptRoot 'Environment.ps1')
Push-Location $taskRoot
try {
    & cargo build --release -p snapdock
    if ($LASTEXITCODE -ne 0) { throw 'Desktop client release build failed' }

    if (-not $SkipServerBuild) {
        & cargo build --release --locked --manifest-path src/server/Cargo.toml
        if ($LASTEXITCODE -ne 0) { throw 'Sync server release build failed' }
    }

    $stageDir = Join-Path $taskRoot 'dist-app\win-unpacked'
    $nativeDir = Join-Path $stageDir 'resources\native'
    $localesDestDir = Join-Path $stageDir 'locales'
    $serverDir = Join-Path $taskRoot 'dist-app\server'
    New-Item -ItemType Directory -Force $stageDir, $nativeDir, $localesDestDir, $serverDir | Out-Null

    Copy-Item -LiteralPath 'target\release\snapdock.exe' -Destination (Join-Path $stageDir 'SnapDock.exe') -Force

    # Copy editable locale translation files alongside the executable (locales/zh-CN.json)
    $localeSource = Join-Path $taskRoot 'src\shared\locales\zh-CN.json'
    if (Test-Path -LiteralPath $localeSource) {
        Copy-Item -LiteralPath $localeSource -Destination (Join-Path $localesDestDir 'zh-CN.json') -Force
        $releaseLocalesDir = Join-Path $taskRoot 'target\release\locales'
        New-Item -ItemType Directory -Force $releaseLocalesDir | Out-Null
        Copy-Item -LiteralPath $localeSource -Destination (Join-Path $releaseLocalesDir 'zh-CN.json') -Force
    }

    # Share the runtime between OCR and translation. Model packs are downloaded in Settings.
    $ocrDllSource = Join-Path $taskRoot '.runtime\ocr\onnxruntime.dll'
    if (Test-Path -LiteralPath $ocrDllSource) {
        Copy-Item -LiteralPath $ocrDllSource -Destination (Join-Path $stageDir 'onnxruntime.dll') -Force
        Copy-Item -LiteralPath $ocrDllSource -Destination (Join-Path $taskRoot 'target\release\onnxruntime.dll') -Force
    }

    if (Test-Path -LiteralPath 'src\server\target\release\snapdock-inspect.exe') {
        Copy-Item -LiteralPath 'src\server\target\release\snapdock-inspect.exe' -Destination (Join-Path $nativeDir 'snapdock-inspect.exe') -Force
    }
    if (Test-Path -LiteralPath 'src\server\target\release\snapdock-server.exe') {
        Copy-Item -LiteralPath 'src\server\target\release\snapdock-server.exe' -Destination (Join-Path $serverDir 'snapdock-server.exe') -Force
    }

    $zipPath = Join-Path $taskRoot 'dist-app\SnapDock-1.0.0-win.zip'
    if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }
    # Stage files may coexist with models downloaded by a user running win-unpacked.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::Open($zipPath, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($entry in @('SnapDock.exe', 'onnxruntime.dll', 'locales/zh-CN.json', 'resources/native/snapdock-inspect.exe')) {
            $file = Join-Path $stageDir $entry
            if (Test-Path -LiteralPath $file -PathType Leaf) {
                [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $file, $entry) | Out-Null
            }
        }
    } finally {
        $archive.Dispose()
    }
    Write-Host "Package created: $zipPath"
} finally { Pop-Location }
