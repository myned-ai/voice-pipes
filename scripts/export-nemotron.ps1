# Wrapper to build + run the Nemotron-export Docker image on Windows.
#
# Usage:
#   ./scripts/export-nemotron.ps1 -NemoFile $env:USERPROFILE\hf-cache\nemotron-streaming\nemotron-speech-streaming-en-0.6b.nemo
#
# Optional args mirror the underlying Python script.
#
# Outputs land in voice-pipes/models/nemotron-streaming/<config-label>/.

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $NemoFile,

    [string] $OutDir = (Join-Path $PSScriptRoot "..\voice-pipes\models\nemotron-streaming"),

    [string[]] $Config = @(),

    [switch] $SkipMel,

    [int] $Opset = 17,

    [string] $ImageTag = "voice-pipes/nemo-export:1",

    [switch] $Rebuild
)

$ErrorActionPreference = "Stop"

# --- Resolve absolute paths (Docker volume mounts must be absolute) ---
$NemoFileAbs = (Resolve-Path -LiteralPath $NemoFile).Path
if (-not (Test-Path -LiteralPath $NemoFileAbs -PathType Leaf)) {
    throw "Nemo file not found: $NemoFileAbs"
}

if (-not (Test-Path -LiteralPath $OutDir)) {
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
}
$OutDirAbs = (Resolve-Path -LiteralPath $OutDir).Path

$NemoDir  = Split-Path -Parent $NemoFileAbs
$NemoName = Split-Path -Leaf   $NemoFileAbs

# --- Build image if missing or -Rebuild ---
$existing = & docker image ls --format "{{.Repository}}:{{.Tag}}" | Where-Object { $_ -eq $ImageTag }
if ($Rebuild -or -not $existing) {
    Write-Host "[export-nemotron] building image $ImageTag ..." -ForegroundColor Cyan
    & docker build `
        -f (Join-Path $PSScriptRoot "Dockerfile.nemo-export") `
        -t $ImageTag `
        $PSScriptRoot
    if ($LASTEXITCODE -ne 0) { throw "docker build failed" }
}

# --- Build container args ---
$dockerArgs = @(
    "run", "--rm",
    "-v", "${OutDirAbs}:/out",
    "-v", "${NemoDir}:/in:ro",
    $ImageTag,
    "--nemo-file", "/in/$NemoName",
    "--out-dir",   "/out",
    "--opset",     "$Opset"
)

foreach ($cfg in $Config) {
    # The Python script expects 'left,chunk,right' (commas). The output
    # directory uses dashes (e.g. `7-10-7`), and users naturally pass that
    # form on the CLI — translate either form transparently here.
    $cfgNormalized = $cfg -replace '-', ','
    $dockerArgs += @("--config", $cfgNormalized)
}
if ($SkipMel) {
    $dockerArgs += "--skip-mel"
}

Write-Host "[export-nemotron] running: docker $($dockerArgs -join ' ')" -ForegroundColor Cyan
& docker @dockerArgs
if ($LASTEXITCODE -ne 0) { throw "export failed (exit $LASTEXITCODE)" }

Write-Host "[export-nemotron] done - output at $OutDirAbs" -ForegroundColor Green
