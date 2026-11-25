param(
    [string]$TargetDir = '',
    [switch]$IncludeCls,
    [string]$OrtVersion = '1.24.4'
)

$ErrorActionPreference = 'Stop'
$taskRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($TargetDir)) {
    $TargetDir = Join-Path $taskRoot '.runtime\ocr'
}
$modelsDir = Join-Path $TargetDir 'models'
New-Item -ItemType Directory -Force $TargetDir, $modelsDir | Out-Null

function Get-FileSha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Download-VerifiedFile([string]$Url, [string]$DestPath, [string]$ExpectedSha256) {
    if (Test-Path -LiteralPath $DestPath) {
        if ([string]::IsNullOrWhiteSpace($ExpectedSha256) -or (Get-FileSha256 $DestPath) -eq $ExpectedSha256) {
            Write-Host "Already exists and verified: $DestPath"
            return
        }
        Write-Warning "Checksum mismatch; re-downloading: $DestPath"
        Remove-Item -LiteralPath $DestPath -Force
    }
    $part = "$DestPath.part"
    if (Test-Path -LiteralPath $part) { Remove-Item -LiteralPath $part -Force }
    Write-Host "Downloading: $Url"
    & curl.exe -fL --retry 3 --connect-timeout 20 -o $part $Url
    if ($LASTEXITCODE -ne 0) {
        if (Test-Path -LiteralPath $part) { Remove-Item -LiteralPath $part -Force }
        throw "Download failed: $Url"
    }
    if (-not [string]::IsNullOrWhiteSpace($ExpectedSha256)) {
        $actual = Get-FileSha256 $part
        if ($actual -ne $ExpectedSha256) {
            Remove-Item -LiteralPath $part -Force
            throw "SHA-256 verification failed ($DestPath): expected $ExpectedSha256, got $actual"
        }
    }
    Move-Item -LiteralPath $part -Destination $DestPath -Force
    Write-Host "Download completed: $DestPath"
}

$assets = @(
    @{
        Name = 'ch_PP-OCRv5_det_mobile.onnx'
        Url  = 'https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv5/det/ch_PP-OCRv5_det_mobile.onnx'
        Sha  = '4d97c44a20d30a81aad087d6a396b08f786c4635742afc391f6621f5c6ae78ae'
    },
    @{
        Name = 'ch_PP-OCRv5_rec_mobile.onnx'
        Url  = 'https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv5/rec/ch_PP-OCRv5_rec_mobile.onnx'
        Sha  = '5825fc7ebf84ae7a412be049820b4d86d77620f204a041697b0494669b1742c5'
    },
    @{
        Name = 'ppocrv5_dict.txt'
        Url  = 'https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/paddle/PP-OCRv5/rec/ch_PP-OCRv5_rec_server/ppocrv5_dict.txt'
        Sha  = 'd1979e9f794c464c0d2e0b70a7fe14dd978e9dc644c0e71f14158cdf8342af1b'
    }
)

if ($IncludeCls) {
    $assets += @{
        Name = 'ch_PP-LCNet_x0_25_textline_ori_cls_mobile.onnx'
        Url  = 'https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv5/cls/ch_PP-LCNet_x0_25_textline_ori_cls_mobile.onnx'
        Sha  = '54379ae5174d026780215fc748a7f31910dee36818e63d49e17dc598ecc82df7'
    }
}

foreach ($item in $assets) {
    Download-VerifiedFile $item.Url (Join-Path $modelsDir $item.Name) $item.Sha
}

$dllDest = Join-Path $TargetDir 'onnxruntime.dll'
if (-not (Test-Path -LiteralPath $dllDest)) {
    $nupkgPath = Join-Path $env:TEMP "Microsoft.ML.OnnxRuntime.$OrtVersion.zip"
    $extractDir = Join-Path $env:TEMP "ort-extract-$OrtVersion"
    if (Test-Path -LiteralPath $extractDir) { Remove-Item -LiteralPath $extractDir -Recurse -Force }
    if (Test-Path -LiteralPath $nupkgPath) { Remove-Item -LiteralPath $nupkgPath -Force }

    $nugetUrls = @(
        "https://www.nuget.org/api/v2/package/Microsoft.ML.OnnxRuntime/$OrtVersion",
        "https://www.nuget.org/api/v2/package/Microsoft.ML.OnnxRuntime/1.24.3",
        "https://www.nuget.org/api/v2/package/Microsoft.ML.OnnxRuntime/1.24.2",
        "https://www.nuget.org/api/v2/package/Microsoft.ML.OnnxRuntime/1.24.1"
    )

    $downloaded = $false
    foreach ($u in $nugetUrls) {
        Write-Host "Downloading ONNX Runtime library: $u"
        & curl.exe -fL --retry 2 --connect-timeout 20 -o $nupkgPath $u
        if ($LASTEXITCODE -eq 0 -and (Test-Path -LiteralPath $nupkgPath)) {
            $downloaded = $true
            break
        }
    }
    if (-not $downloaded) {
        throw 'Failed to download Microsoft.ML.OnnxRuntime 1.24.x runtime library'
    }

    Expand-Archive -LiteralPath $nupkgPath -DestinationPath $extractDir -Force
    $foundDll = Get-ChildItem -Path $extractDir -Recurse -Filter 'onnxruntime.dll' |
        Where-Object { $_.FullName -match 'win-x64' } |
        Select-Object -First 1
    if (-not $foundDll) {
        throw 'win-x64 onnxruntime.dll not found in ONNX Runtime package'
    }
    Copy-Item -LiteralPath $foundDll.FullName -Destination $dllDest -Force
    Remove-Item -LiteralPath $extractDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $nupkgPath -Force -ErrorAction SilentlyContinue
    Write-Host "Runtime library extracted: $dllDest"
} else {
    Write-Host "Runtime library already exists: $dllDest"
}

Write-Host "RapidOCR assets ready: $TargetDir"
