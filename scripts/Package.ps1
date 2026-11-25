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
    $ocrModelsRootDir = Join-Path $stageDir 'models\ocr'
    $localesDestDir = Join-Path $stageDir 'locales'
    $serverDir = Join-Path $taskRoot 'dist-app\server'
    New-Item -ItemType Directory -Force $stageDir, $nativeDir, $ocrModelsRootDir, $localesDestDir, $serverDir | Out-Null

    Copy-Item -LiteralPath 'target\release\snapdock.exe' -Destination (Join-Path $stageDir 'SnapDock.exe') -Force

    # Copy editable locale translation files alongside the executable (locales/zh-CN.json)
    $localeSource = Join-Path $taskRoot 'src\shared\locales\zh-CN.json'
    if (Test-Path -LiteralPath $localeSource) {
        Copy-Item -LiteralPath $localeSource -Destination (Join-Path $localesDestDir 'zh-CN.json') -Force
        $releaseLocalesDir = Join-Path $taskRoot 'target\release\locales'
        New-Item -ItemType Directory -Force $releaseLocalesDir | Out-Null
        Copy-Item -LiteralPath $localeSource -Destination (Join-Path $releaseLocalesDir 'zh-CN.json') -Force
    }

    # Copy local RapidOCR models (models/ocr/<model_id>/ beside executable) and onnxruntime.dll
    $ocrSourceDir = Join-Path $taskRoot '.runtime\ocr'
    $ocrModelsSource = Join-Path $ocrSourceDir 'models'
    $ocrDllSource = Join-Path $ocrSourceDir 'onnxruntime.dll'
    if (Test-Path -LiteralPath $ocrModelsSource) {
        $ocrDefaultPackDir = Join-Path $ocrModelsRootDir 'ppocrv5-ch-mobile'
        New-Item -ItemType Directory -Force $ocrDefaultPackDir | Out-Null
        Copy-Item -Path (Join-Path $ocrModelsSource '*') -Destination $ocrDefaultPackDir -Recurse -Force
        $releaseModelsDir = Join-Path $taskRoot 'target\release\models\ocr\ppocrv5-ch-mobile'
        New-Item -ItemType Directory -Force $releaseModelsDir | Out-Null
        Copy-Item -Path (Join-Path $ocrModelsSource '*') -Destination $releaseModelsDir -Recurse -Force
    }
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
    Compress-Archive -Path (Join-Path $stageDir '*') -DestinationPath $zipPath -Force
    Write-Host "Package created: $zipPath"
} finally { Pop-Location }
