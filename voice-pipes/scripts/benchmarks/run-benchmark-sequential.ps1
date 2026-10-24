<#
.SYNOPSIS
    Sequential benchmark: runs one variant at a time to avoid resource contention.

.DESCRIPTION
    Like run-benchmark-openai-vs-gemma.ps1 but runs variants SEQUENTIALLY:
    only one worker is loaded at a time, so STT/TTS timings are not skewed
    by the other variant's resource usage.

    Flow:
    1. Create experiment + suite (both variants defined)
    2. Start Worker A only -> run variant A -> stop Worker A
    3. Start Worker B only -> run variant B -> stop Worker B
    4. Display combined results

    Uses the ?variants= query parameter to tell the runner which variant
    to execute in each phase.

.PARAMETER OrchestratorUrl
    Base URL of the orchestrator API (default: http://127.0.0.1:9090)

.PARAMETER AudioFiles
    Array of WAV file paths to use as test scenarios.

.PARAMETER Repetitions
    Number of times to repeat each scenario per variant (default: 3)

.PARAMETER TimeoutS
    Per-scenario timeout in seconds (default: 180)

.PARAMETER MaxWaitS
    Maximum time to wait per variant run (default: 600)

.PARAMETER LlmServerUrl
    LLM server health check URL for Gemma 4 (default: http://localhost:8081)

.EXAMPLE
    .\run-benchmark-sequential.ps1

.EXAMPLE
    .\run-benchmark-sequential.ps1 -AudioFiles hello.wav,test_speech.wav -Repetitions 5
#>

param(
    [string]$OrchestratorUrl = "http://127.0.0.1:9090",

    [string]$VariantA_Config       = "examples/test-live-gemma4.toml",
    [string]$VariantA_Name         = "Gemma 4 E2B (local)",
    [string]$VariantA_WorkerConfig = "worker-bench-a.toml",

    [string]$VariantB_Config       = "examples/bench-openai.toml",
    [string]$VariantB_Name         = "OpenAI GPT-4o-mini",
    [string]$VariantB_WorkerConfig = "worker-bench-openai.toml",

    [string[]]$AudioFiles = @("examples/audio/hello.wav", "examples/audio/test_speech.wav"),
    [int]$Repetitions     = 3,
    [int]$TimeoutS        = 180,
    [int]$PollIntervalS   = 5,
    [int]$MaxWaitS        = 600,
    [string]$LlmServerUrl = "http://localhost:8081"
)

$ErrorActionPreference = "Stop"
# Orchestrator admin API needs VP_ADMIN_TOKEN (env or voice-pipes/.env).
. (Join-Path $PSScriptRoot "../../../scripts/admin-auth.ps1")
$api = "$OrchestratorUrl/api/v1"

function Write-Step($msg) { Write-Host "`n==> $msg" -ForegroundColor Cyan }
function Write-Ok($msg)   { Write-Host "    OK: $msg" -ForegroundColor Green }
function Write-Fail($msg) { Write-Host "    FAIL: $msg" -ForegroundColor Red; exit 1 }

function Get-WorkerTags($configPath) {
    if (Test-Path $configPath) {
        $content = Get-Content $configPath -Raw
        $match = [regex]::Match($content, 'tags\s*=\s*\[([^\]]*)\]')
        if ($match.Success) {
            $raw = $match.Groups[1].Value
            return @($raw -split ',' | ForEach-Object { $_.Trim().Trim('"').Trim("'") } | Where-Object { $_ })
        }
    }
    return @()
}

function Stop-WorkerProcess($proc) {
    if ($proc -and -not $proc.HasExited) {
        Write-Host "    Stopping worker PID $($proc.Id)..." -ForegroundColor DarkGray
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        # Wait a moment for the worker to deregister
        Start-Sleep -Seconds 3
    }
}

function Start-WorkerAndWait($configPath, $workerConfigPath, $name, $stderrLog) {
    $binary = "target/release/voice-pipes.exe"
    if (-not (Test-Path $binary)) { $binary = "target/debug/voice-pipes.exe" }
    if (-not (Test-Path $binary)) { Write-Fail "Cannot find voice-pipes binary." }

    # Ensure logs directory exists
    if (-not (Test-Path "logs")) { New-Item -ItemType Directory -Path "logs" | Out-Null }
    $stderrLog = "logs/$stderrLog"

    Write-Host "    Starting worker ($name): $configPath" -ForegroundColor DarkGray
    $proc = Start-Process -FilePath $binary `
        -ArgumentList "serve", "--config", $configPath, "--worker-config", $workerConfigPath `
        -PassThru -NoNewWindow -RedirectStandardError $stderrLog
    Write-Ok "Worker started (PID $($proc.Id))"

    # Wait for registration
    $expectedTag = (Get-WorkerTags $workerConfigPath) | Select-Object -First 1
    Write-Host "    Waiting for tag [$expectedTag] to register..." -ForegroundColor DarkGray
    $maxWait = 90
    $elapsed = 0
    while ($elapsed -lt $maxWait) {
        Start-Sleep -Seconds 2
        $elapsed += 2
        try {
            $resp = Invoke-RestMethod "$api/workers"
            $active = $resp.data.items | Where-Object { $_.status -eq 'active' }
            $found = $active | Where-Object { $_.tags -contains $expectedTag }
            if ($found) {
                Write-Ok "Worker registered with tag [$expectedTag]"
                return $proc
            }
        } catch { }
        Write-Host "    [$elapsed s] Waiting..." -ForegroundColor DarkGray
    }
    Stop-WorkerProcess $proc
    Write-Fail "Worker did not register within ${maxWait}s."
}

function Run-VariantAndPoll($experimentId, $variantId, $variantName) {
    Write-Step "Running variant: $variantName ..."
    try {
        $null = Invoke-RestMethod -Method POST `
            -Uri "$api/benchmark/experiments/$experimentId/run?timeout_s=$TimeoutS&variants=$variantId"
        Write-Ok "Variant run started"
    } catch {
        Write-Fail "Failed to start variant run: $_"
    }

    # Poll for completion
    $elapsed = 0
    $status = "running"
    while ($elapsed -lt $MaxWaitS) {
        Start-Sleep -Seconds $PollIntervalS
        $elapsed += $PollIntervalS
        try {
            $check = Invoke-RestMethod "$api/benchmark/experiments/$experimentId"
            $expData = $check.data
            if (-not $expData) { $expData = $check }
            $status = $expData.status
        } catch {
            Write-Host "    Poll error: $_ (retrying...)" -ForegroundColor Yellow
            continue
        }
        Write-Host "    [$elapsed s] Status: $status" -ForegroundColor DarkGray
        if ($status -eq "complete" -or $status -eq "failed") { break }
    }
    if ($status -eq "running") {
        Write-Fail "Variant $variantName did not complete within ${MaxWaitS}s."
    }
    Write-Ok "Variant $variantName finished (status: $status)"
}

# ── 1. Health checks ────────────────────────────────────────
Write-Step "Checking LLM server (Gemma 4) at $LlmServerUrl ..."
try {
    $llmHealth = Invoke-WebRequest -Uri "$LlmServerUrl/health" -TimeoutSec 5 -ErrorAction Stop
    if ($llmHealth.StatusCode -eq 200) { Write-Ok "Gemma 4 llama-server is running" }
    else { Write-Fail "LLM server responded with status $($llmHealth.StatusCode)" }
} catch {
    Write-Fail "Gemma 4 llama-server not reachable at $LlmServerUrl."
}

Write-Step "Checking OpenAI API key ..."
if (-not $env:OPENAI_API_KEY) { Write-Fail "OPENAI_API_KEY environment variable is not set." }
Write-Ok "OPENAI_API_KEY is set"

Write-Step "Checking orchestrator at $OrchestratorUrl ..."
try {
    $null = Invoke-RestMethod "$api/health"
    Write-Ok "Orchestrator is up"
} catch {
    Write-Fail "Cannot reach orchestrator at $OrchestratorUrl."
}

# ── 2. Validate audio files ─────────────────────────────────
Write-Step "Validating audio files..."
foreach ($file in $AudioFiles) {
    if (-not (Test-Path $file)) { Write-Fail "Audio file not found: $file" }
}
Write-Ok "$($AudioFiles.Count) audio file(s) found"

# ── 3. Create test suite ────────────────────────────────────
Write-Step "Creating test suite..."
$scenarios = @()
foreach ($file in $AudioFiles) {
    $absPath = (Resolve-Path $file).Path
    $scenarioName = [System.IO.Path]::GetFileNameWithoutExtension($file)
    $scenarios += @{ id = $scenarioName; name = $scenarioName; input = @{ type = "audio_file"; path = $absPath } }
}

$suiteBody = @{
    name        = "Sequential OpenAI vs Gemma 4 - $(Get-Date -Format 'yyyy-MM-dd HH:mm')"
    description = "Sequential run: one variant at a time to avoid resource contention."
    scenarios   = $scenarios
} | ConvertTo-Json -Depth 5

try {
    $suite = Invoke-RestMethod -Method POST -Uri "$api/benchmark/suites" `
        -ContentType "application/json" -Body $suiteBody
    $suiteId = $suite.data.id
    if (-not $suiteId) { $suiteId = $suite.id }
    Write-Ok "Suite created: $suiteId"
} catch {
    Write-Fail "Failed to create test suite: $_"
}

# ── 4. Create experiment with both variants ──────────────────
Write-Step "Creating experiment (both variants defined, will run sequentially)..."

$variantIdA = ($VariantA_Name -replace '[^a-zA-Z0-9]', '-').ToLower().TrimEnd('-')
$variantIdB = ($VariantB_Name -replace '[^a-zA-Z0-9]', '-').ToLower().TrimEnd('-')
$tagsA = Get-WorkerTags $VariantA_WorkerConfig
$tagsB = Get-WorkerTags $VariantB_WorkerConfig

Write-Host "    Variant A: $VariantA_Name (id=$variantIdA, tags: $($tagsA -join ', '))" -ForegroundColor DarkGray
Write-Host "    Variant B: $VariantB_Name (id=$variantIdB, tags: $($tagsB -join ', '))" -ForegroundColor DarkGray

$expBody = @{
    name          = "Sequential: OpenAI vs Gemma 4 - $(Get-Date -Format 'yyyy-MM-dd HH:mm')"
    test_suite_id = $suiteId
    variants      = @(
        @{ id = $variantIdA; name = $VariantA_Name; config_path = $VariantA_Config; config_hash = ""; tags = [array]$tagsA },
        @{ id = $variantIdB; name = $VariantB_Name; config_path = $VariantB_Config; config_hash = ""; tags = [array]$tagsB }
    )
    repetitions = $Repetitions
} | ConvertTo-Json -Depth 5

try {
    $exp = Invoke-RestMethod -Method POST -Uri "$api/benchmark/experiments" `
        -ContentType "application/json" -Body $expBody
    $expId = $exp.data.id
    if (-not $expId) { $expId = $exp.id }
    Write-Ok "Experiment created: $expId"
} catch {
    Write-Fail "Failed to create experiment: $_"
}

# ── 5. Run variant A only ──────────────────────────
Write-Step "PHASE 1: $VariantA_Name (only this worker loaded)"

$procA = Start-WorkerAndWait $VariantA_Config $VariantA_WorkerConfig $VariantA_Name "worker-seq-a-stderr.log"
Run-VariantAndPoll $expId $variantIdA $VariantA_Name
Stop-WorkerProcess $procA
Write-Ok "Variant A complete, worker A stopped"

# ── 6. Run variant B only ──────────────────────────
Write-Step "PHASE 2: $VariantB_Name (only this worker loaded)"

$procB = Start-WorkerAndWait $VariantB_Config $VariantB_WorkerConfig $VariantB_Name "worker-seq-b-stderr.log"
Run-VariantAndPoll $expId $variantIdB $VariantB_Name
Stop-WorkerProcess $procB
Write-Ok "Variant B complete, worker B stopped"

# ── 7. Fetch and display results ────────────────────────────
Write-Step "Fetching combined results..."

try {
    $results = Invoke-RestMethod "$api/benchmark/experiments/$expId/results"
    $resData = $results.data
    if (-not $resData) { $resData = $results }
} catch {
    Write-Fail "Failed to fetch results: $_"
}

Write-Host "`n" -NoNewline
Write-Host ("=" * 78) -ForegroundColor White
Write-Host "  SEQUENTIAL BENCHMARK: OPENAI vs GEMMA 4" -ForegroundColor White
Write-Host ("=" * 78) -ForegroundColor White

# Summary comparison table
Write-Host "`n  VARIANT COMPARISON:" -ForegroundColor Yellow
Write-Host ("  " + "-" * 74) -ForegroundColor DarkGray

$colA = $VariantA_Name.Substring(0, [Math]::Min(20, $VariantA_Name.Length))
$colB = $VariantB_Name.Substring(0, [Math]::Min(20, $VariantB_Name.Length))
$header = '  {0,-25} {1,20} {2,20} {3,10}' -f 'Metric', $colA, $colB, 'Delta'
Write-Host $header -ForegroundColor White
Write-Host ("  " + "-" * 74) -ForegroundColor DarkGray

$vrA = $resData.variant_results | Where-Object { $_.variant_name -eq $VariantA_Name }
$vrB = $resData.variant_results | Where-Object { $_.variant_name -eq $VariantB_Name }

function Format-Delta($a, $b) {
    if (-not $a -or -not $b -or $a -eq 0) { return '---' }
    $pct = (($b - $a) / $a) * 100
    $sign = if ($pct -gt 0) { '+' } else { '' }
    return ('{0}{1}%' -f $sign, [math]::Round($pct, 1))
}

function Write-MetricRow($label, $valA, $valB, $unit = 'ms', $lowerIsBetter = $true) {
    $fmtA = if ($null -ne $valA) { '{0} {1}' -f [math]::Round($valA, 1), $unit } else { '---' }
    $fmtB = if ($null -ne $valB) { '{0} {1}' -f [math]::Round($valB, 1), $unit } else { '---' }
    $delta = Format-Delta $valA $valB
    $color = 'White'
    if ($null -ne $valA -and $null -ne $valB -and $valA -gt 0) {
        $diff = $valB - $valA
        if ($lowerIsBetter) {
            $color = if ($diff -lt 0) { 'Green' } elseif ($diff -gt 0) { 'Red' } else { 'White' }
        } else {
            $color = if ($diff -gt 0) { 'Green' } elseif ($diff -lt 0) { 'Red' } else { 'White' }
        }
    }
    $line = '  {0,-25} {1,20} {2,20} {3,10}' -f $label, $fmtA, $fmtB, $delta
    Write-Host $line -ForegroundColor $color
}

if ($vrA -and $vrB) {
    $sA = $vrA.summary
    $sB = $vrB.summary
    Write-MetricRow "Avg E2E Latency"  $sA.avg_e2e_latency_ms $sB.avg_e2e_latency_ms
    Write-MetricRow "P50 E2E Latency"  $sA.p50_e2e_latency_ms $sB.p50_e2e_latency_ms
    Write-MetricRow "P90 E2E Latency"  $sA.p90_e2e_latency_ms $sB.p90_e2e_latency_ms
    Write-MetricRow "P99 E2E Latency"  $sA.p99_e2e_latency_ms $sB.p99_e2e_latency_ms
    Write-MetricRow "Avg TTFT"         $sA.avg_ttft_ms          $sB.avg_ttft_ms
    Write-MetricRow "Avg TTFB"         $sA.avg_ttfb_ms          $sB.avg_ttfb_ms
    Write-MetricRow "Pass Rate"        ($sA.scenarios_passed / [Math]::Max($sA.scenarios_run, 1) * 100) `
                                       ($sB.scenarios_passed / [Math]::Max($sB.scenarios_run, 1) * 100) "%" $false
    Write-Host ("  " + "-" * 74) -ForegroundColor DarkGray
} else {
    Write-Host '  Could not compare -- missing variant results' -ForegroundColor Yellow
}

# Per-variant detail
foreach ($vr in $resData.variant_results) {
    Write-Host "`n  Variant: $($vr.variant_name)" -ForegroundColor Yellow
    $s = $vr.summary
    Write-Host "    Scenarios: $($s.scenarios_passed) passed / $($s.scenarios_failed) failed / $($s.scenarios_run) total"
    if ($s.avg_e2e_latency_ms) { Write-Host "    Avg E2E Latency:  $([math]::Round($s.avg_e2e_latency_ms, 1)) ms" }
    if ($s.p50_e2e_latency_ms) { Write-Host "    P50 E2E Latency:  $($s.p50_e2e_latency_ms) ms" }
    if ($s.p90_e2e_latency_ms) { Write-Host "    P90 E2E Latency:  $($s.p90_e2e_latency_ms) ms" }
    if ($s.avg_ttft_ms)        { Write-Host "    Avg TTFT:         $([math]::Round($s.avg_ttft_ms, 1)) ms" }
    if ($s.avg_ttfb_ms)        { Write-Host "    Avg TTFB:         $([math]::Round($s.avg_ttfb_ms, 1)) ms" }

    Write-Host "`n    Scenario Details:" -ForegroundColor DarkYellow
    foreach ($sc in $vr.scenarios) {
        $icon = if ($sc.status -eq "passed") { "[PASS]" } else { "[FAIL]" }
        $color = if ($sc.status -eq "passed") { "Green" } else { "Red" }
        Write-Host "      $icon $($sc.scenario_name)" -ForegroundColor $color
        $m = $sc.metrics
        if ($m.stt_text)       { Write-Host ('           STT:     "{0}"' -f $m.stt_text) }
        if ($m.response_text)  { Write-Host ('           LLM:     "{0}"...' -f $m.response_text.Substring(0, [Math]::Min(100, $m.response_text.Length))) }
        if ($m.e2e_latency_ms) { Write-Host ('           E2E:     {0} ms' -f [math]::Round($m.e2e_latency_ms, 1)) }
        if ($m.ttft_ms)        { Write-Host ('           TTFT:    {0} ms' -f [math]::Round($m.ttft_ms, 1)) }
        if ($m.ttfb_ms)        { Write-Host ('           TTFB:    {0} ms' -f [math]::Round($m.ttfb_ms, 1)) }
    }
}

Write-Host "`n$("=" * 78)" -ForegroundColor White
Write-Host "  Experiment ID: $expId"
Write-Host "  Full JSON:     $api/benchmark/experiments/$expId/results"
Write-Host "  Admin UI:      http://localhost:5173/benchmarks/$expId"
Write-Host ("=" * 78) -ForegroundColor White

# Save raw JSON
$resultsDir = "benchmarks/results"
if (-not (Test-Path $resultsDir)) { New-Item -ItemType Directory -Path $resultsDir -Force | Out-Null }
$outFile = "$resultsDir/benchmark-sequential-$(Get-Date -Format 'yyyyMMdd-HHmmss').json"
$resData | ConvertTo-Json -Depth 15 | Set-Content $outFile -Encoding UTF8
Write-Host "`n  Results saved to $outFile" -ForegroundColor Green

Write-Host ''
Write-Host '  Done! Open the Admin UI to see the comparison charts.' -ForegroundColor Green
Write-Host '  (Sequential mode: STT/TTS metrics are isolated per variant)' -ForegroundColor DarkGray
Write-Host ''
