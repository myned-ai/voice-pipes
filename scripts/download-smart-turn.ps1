# Download the Pipecat Smart Turn ONNX model for use with the `turn-smart`
# feature (see src/stt/turn_smart.rs).
#
# The Pipecat HuggingFace repo `pipecat-ai/smart-turn-v3` publishes several
# ONNX variants. Default is `smart-turn-v3.2-cpu.onnx` (~8.7 MB, int8
# quantized) which runs in tens of milliseconds on CPU and is the recommended
# default for local deployments.
#
# Usage:
#   .\scripts\download-smart-turn.ps1
#   .\scripts\download-smart-turn.ps1 -Variant v3.2-gpu
#   .\scripts\download-smart-turn.ps1 -OutDir models -Force
#
# Reference: https://huggingface.co/pipecat-ai/smart-turn-v3
# License: BSD-2-Clause (model), Apache-2.0 (training/inference code)

[CmdletBinding()]
param(
    [ValidateSet('v3.0', 'v3.1-cpu', 'v3.1-gpu', 'v3.2-cpu', 'v3.2-gpu')]
    [string] $Variant = 'v3.2-cpu',

    [string] $OutDir = 'models',

    [switch] $Force
)

$ErrorActionPreference = 'Stop'

$fileName = "smart-turn-$Variant.onnx"
$url      = "https://huggingface.co/pipecat-ai/smart-turn-v3/resolve/main/$fileName"

if (-not (Test-Path -LiteralPath $OutDir)) {
    New-Item -ItemType Directory -Path $OutDir | Out-Null
}

$destPath = Join-Path -Path $OutDir -ChildPath $fileName

if ((Test-Path -LiteralPath $destPath) -and -not $Force) {
    Write-Host "Already present: $destPath  (use -Force to re-download)" -ForegroundColor Yellow
    Write-Host ""
    Write-Host "Add to your TOML:" -ForegroundColor Cyan
    Write-Host "  [turn_detection]"
    Write-Host "  mode               = `"smart`""
    Write-Host "  silence_timeout_ms = 2000"
    Write-Host "  smart_model_path   = `"$destPath`""
    exit 0
}

Write-Host "Downloading $fileName from HuggingFace..." -ForegroundColor Cyan
Write-Host "  $url"
Write-Host "  -> $destPath"

# Prefer the modern progress UI; suppress the slow legacy Invoke-WebRequest progress bar.
$prevProgress = $ProgressPreference
$ProgressPreference = 'SilentlyContinue'
try {
    Invoke-WebRequest -Uri $url -OutFile $destPath -UseBasicParsing
}
finally {
    $ProgressPreference = $prevProgress
}

$sizeMb = [math]::Round((Get-Item -LiteralPath $destPath).Length / 1MB, 2)
Write-Host "Downloaded $sizeMb MB" -ForegroundColor Green
Write-Host ""
Write-Host "Build with the feature flag:" -ForegroundColor Cyan
Write-Host "  cargo build --release -p voice-pipes-cli --features stt-nemotron,turn-smart"
Write-Host ""
Write-Host "Add to your TOML:" -ForegroundColor Cyan
Write-Host "  [turn_detection]"
Write-Host "  mode               = `"smart`""
Write-Host "  silence_timeout_ms = 2000"
Write-Host "  smart_model_path   = `"$destPath`""
Write-Host "  smart_threshold    = 0.5"
Write-Host "  smart_window_secs  = 8"
Write-Host "  smart_min_audio_ms = 500"
