# Gemma 4 live pipeline — downloads the model, starts llama-server, runs voice-pipes.
#
# Prerequisites:
#   - huggingface-cli  (pip install huggingface-hub)
#   - llama-server     (https://github.com/ggerganov/llama.cpp/releases)
#
# Usage:
#   .\run-live-gemma4.ps1                # default: E2B (smallest, ~3 GB)
#   .\run-live-gemma4.ps1 -Model e4b     # E4B (4.5B effective, ~5 GB)
#   .\run-live-gemma4.ps1 -Model 12b     # 12B dense Unified (~10 GB VRAM)
#   .\run-live-gemma4.ps1 -Model 26b     # 26B-A4B MoE (~15 GB VRAM)
#   .\run-live-gemma4.ps1 -Model 31b     # 31B dense (~18 GB VRAM)
#   .\run-live-gemma4.ps1 -CpuOnly       # no GPU, CPU inference
#   .\run-live-gemma4.ps1 -GpuLayers 20  # partial GPU offload

param(
    [ValidateSet("e2b", "e4b", "12b", "26b", "31b")]
    [string]$Model = "e2b",

    [int]$GpuLayers = 99,

    [switch]$CpuOnly,

    [int]$ContextSize = 4096,

    [int]$ServerPort = 8081
)

$ErrorActionPreference = "Stop"

# ── Model lookup table ──────────────────────────────────────
$models = @{
    "e2b" = @{
        repo = "unsloth/gemma-4-E2B-it-GGUF"
        file = "gemma-4-E2B-it-Q4_K_M.gguf"
        dir  = "models/gemma4-e2b"
        desc = "Gemma 4 E2B (2.3B effective, ~3 GB)"
    }
    "e4b" = @{
        repo = "unsloth/gemma-4-E4B-it-GGUF"
        file = "gemma-4-E4B-it-Q4_K_M.gguf"
        dir  = "models/gemma4-e4b"
        desc = "Gemma 4 E4B (4.5B effective, ~5 GB)"
    }
    "12b" = @{
        # Lowercase "12b" matches the unsloth repo/filename casing
        # (unlike the uppercase E2B/E4B repos).
        repo = "unsloth/gemma-4-12b-it-GGUF"
        file = "gemma-4-12b-it-Q4_K_M.gguf"
        dir  = "models/gemma4-12b"
        desc = "Gemma 4 12B dense Unified (~7.1 GB, ~10 GB VRAM)"
    }
    "26b" = @{
        repo = "unsloth/gemma-4-26B-A4B-it-GGUF"
        file = "gemma-4-26B-A4B-it-Q4_K_M.gguf"
        dir  = "models/gemma4-26b"
        desc = "Gemma 4 26B-A4B MoE (3.8B active, ~15 GB)"
    }
    "31b" = @{
        repo = "unsloth/gemma-4-31B-it-GGUF"
        file = "gemma-4-31B-it-Q4_K_M.gguf"
        dir  = "models/gemma4-31b"
        desc = "Gemma 4 31B dense (~18 GB)"
    }
}

$m = $models[$Model]
$modelPath = Join-Path $m.dir $m.file

Write-Host "=== $($m.desc) ===" -ForegroundColor Cyan

# ── Download model if missing ───────────────────────────────
if (-not (Test-Path $modelPath)) {
    Write-Host "Downloading $($m.file) ..." -ForegroundColor Yellow
    New-Item -ItemType Directory -Force -Path $m.dir | Out-Null
    hf download $m.repo $m.file --local-dir $m.dir
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Model download failed. Install huggingface-cli: pip install huggingface-hub"
        exit 1
    }
}

Write-Host "Model ready: $modelPath" -ForegroundColor Green

# ── Resolve GPU layers ──────────────────────────────────────
$ngl = if ($CpuOnly) { 0 } else { $GpuLayers }
Write-Host "GPU layers: $ngl  (use -CpuOnly for CPU-only, -GpuLayers N for partial offload)" -ForegroundColor DarkGray

# ── Start llama-server in background ────────────────────────
Write-Host "Starting llama-server on port $ServerPort ..." -ForegroundColor Yellow

$llamaArgs = @(
    "-m", $modelPath,
    "--port", $ServerPort,
    "-ngl", $ngl,
    "-c", $ContextSize,
    # Required for tool calling: without --jinja llama-server silently
    # ignores the OpenAI `tools` field.
    "--jinja",
    "--log-disable"
)

$serverProc = Start-Process -FilePath "llama-server" -ArgumentList $llamaArgs -PassThru -NoNewWindow

# Wait for server to be ready
$maxWait = 180
$waited = 0
Write-Host "Waiting for llama-server to be ready (up to ${maxWait}s) ..." -NoNewline
while ($waited -lt $maxWait) {
    try {
        $response = Invoke-WebRequest -Uri "http://localhost:$ServerPort/health" -TimeoutSec 2 -ErrorAction SilentlyContinue
        if ($response.StatusCode -eq 200) { break }
    } catch {}
    Start-Sleep -Seconds 1
    $waited++
    Write-Host "." -NoNewline
}
Write-Host ""

if ($waited -ge $maxWait) {
    Write-Error "llama-server failed to start within ${maxWait}s. Check that llama-server is installed."
    if (-not $serverProc.HasExited) { $serverProc.Kill() }
    exit 1
}

Write-Host "llama-server ready!" -ForegroundColor Green

# ── Run voice-pipes ─────────────────────────────────────────
try {
    cargo run --release --features=stt-parakeet,tts-piper,llm-openai-compat,vad-silero -- --config examples/dev/test-live-gemma4.toml live
} finally {
    # Clean up llama-server on exit
    Write-Host "`nStopping llama-server ..." -ForegroundColor Yellow
    if (-not $serverProc.HasExited) {
        $serverProc.Kill()
        $serverProc.WaitForExit(5000) | Out-Null
    }
    Write-Host "Done." -ForegroundColor Green
}
