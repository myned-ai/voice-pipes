<#
.SYNOPSIS
    Run the synthetic-caller mock benchmark (no external APIs).

.DESCRIPTION
    Submits `examples/benchmark/synthetic-caller-mock.json` as a suite and
    runs it against the worker pointed at by the orchestrator. This is a
    diagnostic for the synthetic-caller runner loop itself: if multi-turn
    works here but fails with OpenAI Realtime, the bug is in the OpenAI
    audio-bridge path. If multi-turn fails here too, the bug is in the
    runner loop or stop-condition logic.

    Prerequisites:
      - Orchestrator running (`voice-pipes.exe orchestrate`)
      - At least one worker registered (`voice-pipes.exe serve --worker-config worker.toml`)
      - llama-server (or whatever LLM the variant config points at) running

.EXAMPLE
    .\run-benchmark-mock.ps1
#>
param(
    [string]$OrchestratorUrl = "http://127.0.0.1:9090",
    [string]$VariantName     = "mock-smoke",
    [string]$ConfigPath      = "examples/dev/test-live-gemma4.toml",
    [int]$Repetitions        = 1,
    [int]$TimeoutS           = 60,
    [int]$PollIntervalS      = 3,
    [int]$MaxWaitS           = 180
)

$ErrorActionPreference = "Stop"
# Orchestrator admin API needs VP_ADMIN_TOKEN (env or voice-pipes/.env).
. (Join-Path $PSScriptRoot "../admin-auth.ps1")
$api = "$OrchestratorUrl/api/v1"

function Write-Step($m) { Write-Host "`n==> $m" -ForegroundColor Cyan }
function Write-Ok($m)   { Write-Host "    OK: $m"   -ForegroundColor Green }
function Write-Fail($m) { Write-Host "    FAIL: $m" -ForegroundColor Red; exit 1 }

# Health checks
Write-Step "Checking orchestrator at $OrchestratorUrl ..."
try   { Invoke-RestMethod "$api/health" | Out-Null; Write-Ok "orchestrator up" }
catch { Write-Fail "orchestrator not reachable: $_" }

Write-Step "Checking workers ..."
try {
    $workers = Invoke-RestMethod "$api/workers"
    $total = $workers.data.total
    if ($total -lt 1) { Write-Fail "no workers registered" }
    Write-Ok "$total worker(s) registered"
} catch { Write-Fail "worker query failed: $_" }

# Load the mock suite JSON
$suitePath = "examples/benchmark/synthetic-caller-mock.json"
if (-not (Test-Path $suitePath)) { Write-Fail "suite file missing: $suitePath" }
$suiteJson = Get-Content $suitePath -Raw -Encoding UTF8

Write-Step "Creating suite from $suitePath ..."
try {
    $suite = Invoke-RestMethod -Method POST -Uri "$api/benchmark/suites" `
        -ContentType "application/json" -Body $suiteJson
    $suiteId = $suite.data.id
    if (-not $suiteId) { $suiteId = $suite.id }
    Write-Ok "suite id: $suiteId"
} catch { Write-Fail "suite create failed: $_" }

# Experiment
$variantId = ($VariantName -replace '\s+', '-').ToLower()
$expBody = @{
    name          = "Mock smoke $(Get-Date -Format 'yyyy-MM-dd HH:mm')"
    test_suite_id = $suiteId
    variants      = @(@{
        id          = $variantId
        name        = $VariantName
        config_path = $ConfigPath
        config_hash = ""
        tags        = @()
    })
    repetitions = $Repetitions
} | ConvertTo-Json -Depth 5

Write-Step "Creating experiment ..."
try {
    $exp = Invoke-RestMethod -Method POST -Uri "$api/benchmark/experiments" `
        -ContentType "application/json" -Body $expBody
    $expId = $exp.data.id
    if (-not $expId) { $expId = $exp.id }
    Write-Ok "experiment id: $expId"
} catch { Write-Fail "experiment create failed: $_" }

Write-Step "Starting experiment run ..."
try {
    Invoke-RestMethod -Method POST -Uri "$api/benchmark/experiments/$expId/run?timeout_s=$TimeoutS" | Out-Null
    Write-Ok "started"
} catch { Write-Fail "run start failed: $_" }

Write-Step "Polling for completion (every ${PollIntervalS}s, max ${MaxWaitS}s) ..."
$elapsed = 0
$status  = "running"
while ($elapsed -lt $MaxWaitS) {
    Start-Sleep -Seconds $PollIntervalS
    $elapsed += $PollIntervalS
    try {
        $check   = Invoke-RestMethod "$api/benchmark/experiments/$expId"
        $expData = $check.data; if (-not $expData) { $expData = $check }
        $status  = $expData.status
    } catch { continue }
    Write-Host "    [$elapsed s] status: $status" -ForegroundColor DarkGray
    if ($status -eq "complete" -or $status -eq "failed") { break }
}
if ($status -eq "running") { Write-Fail "experiment did not complete in ${MaxWaitS}s" }

Write-Step "Fetching results ..."
$results = Invoke-RestMethod "$api/benchmark/experiments/$expId/results"
$resData = $results.data; if (-not $resData) { $resData = $results }

Write-Host "`n=== RESULT SUMMARY ===" -ForegroundColor White
foreach ($vr in $resData.variant_results) {
    Write-Host "`nVariant: $($vr.variant_name)" -ForegroundColor Yellow
    foreach ($sc in $vr.scenarios) {
        $color = if ($sc.status -eq "passed") { "Green" } else { "Red" }
        Write-Host "  [$($sc.status)] $($sc.scenario_name)" -ForegroundColor $color

        # Synthetic-caller specific: show turn count + per-turn speakers
        $sr = $sc.metrics.synthetic_caller_result
        if ($sr) {
            Write-Host ("    completed_turns: {0}" -f $sr.completed_turns)
            Write-Host ("    stop_reason:     {0}" -f $sr.stop_reason)
            Write-Host ("    total_duration:  {0} ms" -f $sr.total_duration_ms)
            if ($sr.turns) {
                $i = 0
                foreach ($t in $sr.turns) {
                    $i++
                    $txt = $t.transcript
                    if ($txt -and $txt.Length -gt 80) { $txt = $txt.Substring(0,80) + "..." }
                    Write-Host ("      turn{0,2} [{1,-8}] {2}" -f $i, $t.speaker, $txt)
                }
            }
        }
        if ($sc.expectation_failures) {
            foreach ($ef in $sc.expectation_failures) {
                Write-Host "    expect_fail: $ef" -ForegroundColor Red
            }
        }
    }
}

$out = "benchmark-mock-$(Get-Date -Format 'yyyyMMdd-HHmmss').json"
$resData | ConvertTo-Json -Depth 20 | Set-Content $out -Encoding UTF8
Write-Host "`nFull JSON saved to $out`n" -ForegroundColor Green
