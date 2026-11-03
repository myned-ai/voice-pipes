<#
.SYNOPSIS
    Run the Nemotron streaming-WER harness against one or more model dirs.

.DESCRIPTION
    Builds and runs the `wer_eval` example of voice-pipes-stt-nemotron over a labeled manifest,
    reporting batch/stream WER, CER, RTFx and the batch-to-stream factor
    (BSF). This is the gate for the MatMulNBits self-quant work:
    run it once on the FP32 encoder to
    capture a baseline, then again on `encoder.int4.onnx` and diff the WER.

    Pass multiple -ModelDir values to A/B several quant levels in one go;
    each gets its own JSON under -OutDir.

.PARAMETER ModelDir
    One or more Nemotron model directories (each must contain encoder.onnx,
    decoder_joint.onnx / decoder.onnx+joint.onnx, vocab, export_metadata.json).
    Default: ../../models/nemotron-streaming/7-10-7

.PARAMETER Manifest
    TAB-separated manifest: `<wav_path>\t<reference transcript>` per line.
    Relative WAV paths resolve against the manifest's directory.
    Default: ../plugins/stt/nemotron/examples/wer/manifest.example.tsv

.PARAMETER OutDir
    Directory to write per-model JSON results into. Default: ./wer-results

.PARAMETER Release
    Build/run in release mode (strongly recommended — FP32 is slow in debug).

.EXAMPLE
    ./run-nemotron-wer.ps1 -Release `
        -ModelDir models/nemotron-streaming/7-10-7 `
        -ModelDir models/nemotron-streaming/7-10-7-int4 `
        -Manifest C:\corpora\librispeech-mini\manifest.tsv
#>
[CmdletBinding()]
param(
    # Paths are resolved from the voice-pipes crate root (the script cd's
    # there before invoking cargo).
    [string[]]$ModelDir = @("models/nemotron-streaming/7-10-7"),
    [string]$Manifest = "../plugins/stt/nemotron/examples/wer/manifest.example.tsv",
    [string]$OutDir = "./wer-results",
    [switch]$Release
)

$ErrorActionPreference = "Stop"
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
# voice-pipes crate root is two levels up from scripts/benchmarks/.
$crateDir = Resolve-Path (Join-Path $scriptDir "../../voice-pipes")
Push-Location $crateDir
try {
    $profileArgs = @()
    if ($Release) { $profileArgs += "--release" }

    Write-Host "Building wer_eval..." -ForegroundColor Cyan
    & cargo build @profileArgs -p voice-pipes-stt-nemotron --example wer_eval
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

    foreach ($md in $ModelDir) {
        $label = Split-Path -Leaf $md
        $jsonOut = Join-Path $OutDir "wer-$label.json"
        Write-Host "`n=== $md ===" -ForegroundColor Green
        & cargo run @profileArgs -p voice-pipes-stt-nemotron --example wer_eval -- `
            $md $Manifest $jsonOut
        if ($LASTEXITCODE -ne 0) { throw "eval failed for $md" }
    }

    Write-Host "`nResults written under $OutDir" -ForegroundColor Cyan
}
finally {
    Pop-Location
}
