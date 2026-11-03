<#
.SYNOPSIS
    Run a multi-variant benchmark comparing different pipeline configurations.

.DESCRIPTION
    This script:
    1. Checks that the orchestrator and LLM server are reachable
    2. Starts two worker processes (one per variant) with different configs and tags
    3. Waits for both workers to register with the orchestrator
    4. Creates a test suite with audio scenarios
    5. Creates a multi-variant experiment
    6. Runs the experiment and polls for completion
    7. Prints comparative results
    8. Stops the worker processes

    Each variant is a different pipeline configuration dispatched to its own
    tagged worker. The benchmark runner executes every variant × scenario × rep
    and collects side-by-side metrics.

.PARAMETER OrchestratorUrl
    Base URL of the orchestrator API (default: http://127.0.0.1:9090)

.PARAMETER VariantA_Config
    Pipeline TOML for variant A (default: examples/dev/test-live-gemma4.toml)

.PARAMETER VariantA_Name
    Friendly name for variant A (default: "Gemma 4 Default")

.PARAMETER VariantA_WorkerConfig
    Worker TOML for variant A (default: worker-bench-a.toml)

.PARAMETER VariantB_Config
    Pipeline TOML for variant B (default: examples/bench/bench-gemma4-concise.toml)

.PARAMETER VariantB_Name
    Friendly name for variant B (default: "Gemma 4 Concise")

.PARAMETER VariantB_WorkerConfig
    Worker TOML for variant B (default: worker-bench-b.toml)

.PARAMETER AudioFiles
    Array of WAV file paths to use as test scenarios. Defaults to hello.wav.

.PARAMETER Repetitions
    Number of times to repeat each scenario per variant (default: 3)

.PARAMETER TimeoutS
    Per-scenario timeout in seconds (default: 120)

.PARAMETER MaxWaitS
    Maximum time to wait for experiment completion in seconds (default: 600)

.PARAMETER LlmServerUrl
    LLM server health check URL (default: http://localhost:8081)

.PARAMETER SkipWorkers
    If set, assumes workers are already running and registered. Useful when
    managing workers manually.

.EXAMPLE
    .\run-benchmark-multi.ps1

.EXAMPLE
    .\run-benchmark-multi.ps1 -AudioFiles hello.wav,test_speech.wav -Repetitions 5

.EXAMPLE
    .\run-benchmark-multi.ps1 -VariantB_Config examples/dev/test-live-openai.toml -VariantB_Name "OpenAI GPT-4o-mini"
#>

param(
    [string]$OrchestratorUrl = "http://127.0.0.1:9090",

    [string]$VariantA_Config = "examples/dev/test-live-gemma4.toml",
    [string]$VariantA_Name   = "Gemma 4 Default",
    [string]$VariantA_WorkerConfig = "worker-bench-a.toml",

    [string]$VariantB_Config = "examples/bench/bench-gemma4-concise.toml",
    [string]$VariantB_Name   = "Gemma 4 Concise",
    [string]$VariantB_WorkerConfig = "worker-bench-b.toml",

    [string[]]$AudioFiles = @("examples/audio/hello.wav"),
    [int]$Repetitions     = 3,
    [int]$TimeoutS        = 120,
    [int]$PollIntervalS   = 5,
    [int]$MaxWaitS        = 600,
    [string]$LlmServerUrl = "http://localhost:8081",
    [switch]$SkipWorkers
)

$ErrorActionPreference = "Stop"
# Orchestrator admin API needs VP_ADMIN_TOKEN (env or voice-pipes/.env).
. (Join-Path $PSScriptRoot "../admin-auth.ps1")
$api = "$OrchestratorUrl/api/v1"

function Write-Step($msg) { Write-Host "`n==> $msg" -ForegroundColor Cyan }
function Write-Ok($msg)   { Write-Host "    OK: $msg" -ForegroundColor Green }
function Write-Fail($msg) { Write-Host "    FAIL: $msg" -ForegroundColor Red; exit 1 }

$workerProcesses = @()

function Cleanup-Workers {
    foreach ($proc in $script:workerProcesses) {
        if ($proc -and -not $proc.HasExited) {
            Write-Host "    Stopping worker PID $($proc.Id)..." -ForegroundColor DarkGray
            Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        }
    }
}

# Register cleanup on script exit
trap { Cleanup-Workers }

# ── 1. Health checks ────────────────────────────────────────
Write-Step "Checking LLM server at $LlmServerUrl ..."
try {
    $llmHealth = Invoke-WebRequest -Uri "$LlmServerUrl/health" -TimeoutSec 3 -ErrorAction Stop
    if ($llmHealth.StatusCode -eq 200) {
        Write-Ok "LLM server is running"
    } else {
        Write-Fail "LLM server responded with status $($llmHealth.StatusCode)"
    }
} catch {
    Write-Fail "LLM server not reachable at $LlmServerUrl. Start llama-server first."
}

Write-Step "Checking orchestrator at $OrchestratorUrl ..."
try {
    $health = Invoke-RestMethod "$api/health"
    Write-Ok "Orchestrator is up"
} catch {
    Write-Fail "Cannot reach orchestrator at $OrchestratorUrl. Is it running?"
}

# ── 2. Start worker processes ───────────────────────────────
if (-not $SkipWorkers) {
    Write-Step "Starting worker processes..."

    # Find the binary
    $binary = "target/release/voice-pipes.exe"
    if (-not (Test-Path $binary)) {
        $binary = "target/debug/voice-pipes.exe"
    }
    if (-not (Test-Path $binary)) {
        Write-Fail "Cannot find voice-pipes binary. Run: cargo build --release --features=llm-openai-compat"
    }

    # Ensure logs directory exists
    if (-not (Test-Path "logs")) { New-Item -ItemType Directory -Path "logs" | Out-Null }

    # Worker A
    Write-Host "    Starting Worker A ($VariantA_Name): $VariantA_Config" -ForegroundColor DarkGray
    $procA = Start-Process -FilePath $binary `
        -ArgumentList "serve", "--config", $VariantA_Config, "--worker-config", $VariantA_WorkerConfig `
        -PassThru -NoNewWindow -RedirectStandardError "logs/worker-a-stderr.log"
    $workerProcesses += $procA
    Write-Ok "Worker A started (PID $($procA.Id))"

    # Worker B
    Write-Host "    Starting Worker B ($VariantB_Name): $VariantB_Config" -ForegroundColor DarkGray
    $procB = Start-Process -FilePath $binary `
        -ArgumentList "serve", "--config", $VariantB_Config, "--worker-config", $VariantB_WorkerConfig `
        -PassThru -NoNewWindow -RedirectStandardError "logs/worker-b-stderr.log"
    $workerProcesses += $procB
    Write-Ok "Worker B started (PID $($procB.Id))"

    # Wait for both workers to register with correct tags
    Write-Step "Waiting for tagged workers to register with orchestrator..."
    $maxRegWait = 60
    $regElapsed = 0
    while ($regElapsed -lt $maxRegWait) {
        Start-Sleep -Seconds 2
        $regElapsed += 2
        try {
            $workersResp = Invoke-RestMethod "$api/workers"
            $activeWorkers = $workersResp.data.items | Where-Object { $_.status -eq 'active' }
            $hasTagA = $activeWorkers | Where-Object { $_.tags -contains 'variant:default' }
            $hasTagB = $activeWorkers | Where-Object { $_.tags -contains 'variant:concise' }
            $activeCount = @($activeWorkers).Count
            Write-Host "    [$regElapsed s] Active workers: $activeCount (tagA: $(@($hasTagA).Count), tagB: $(@($hasTagB).Count))" -ForegroundColor DarkGray
            if ($hasTagA -and $hasTagB) {
                Write-Ok "Both tagged workers registered and active"
                break
            }
        } catch {
            Write-Host "    [$regElapsed s] Waiting..." -ForegroundColor DarkGray
        }
    }
    if ($regElapsed -ge $maxRegWait) {
        Cleanup-Workers
        Write-Fail "Tagged workers did not register within ${maxRegWait}s. Check worker logs."
    }
} else {
    Write-Step "Skipping worker startup (--SkipWorkers). Checking existing workers..."
    try {
        $workers = Invoke-RestMethod "$api/workers"
        $total = $workers.data.total
        if ($total -lt 2) {
            Write-Fail "Need at least 2 workers for multi-variant benchmark. Found: $total"
        }
        Write-Ok "$total workers registered"
    } catch {
        Write-Fail "Failed to query workers: $_"
    }
}

# ── 3. Create test suite ────────────────────────────────────
Write-Step "Creating test suite..."

$scenarios = @()
foreach ($file in $AudioFiles) {
    $resolved = (Resolve-Path $file -ErrorAction SilentlyContinue)
    if (-not $resolved) {
        Cleanup-Workers
        Write-Fail "Audio file not found: $file"
    }
    $absPath = $resolved.Path
    $scenarioName = [System.IO.Path]::GetFileNameWithoutExtension($file)
    $scenarios += @{
        id    = $scenarioName
        name  = $scenarioName
        input = @{
            type = "audio_file"
            path = $absPath
        }
    }
}

$suiteBody = @{
    name        = "Multi-Variant Suite $(Get-Date -Format 'yyyy-MM-dd HH:mm')"
    description = "Auto-generated by run-benchmark-multi.ps1"
    scenarios   = $scenarios
} | ConvertTo-Json -Depth 5

try {
    $suite = Invoke-RestMethod -Method POST -Uri "$api/benchmark/suites" `
        -ContentType "application/json" -Body $suiteBody
    $suiteId = $suite.data.id
    if (-not $suiteId) { $suiteId = $suite.id }
    Write-Ok "Suite created: $suiteId"
} catch {
    Cleanup-Workers
    Write-Fail "Failed to create test suite: $_"
}

# ── 4. Create multi-variant experiment ──────────────────────
Write-Step "Creating multi-variant experiment..."

$variantIdA = ($VariantA_Name -replace '\s+', '-').ToLower()
$variantIdB = ($VariantB_Name -replace '\s+', '-').ToLower()

# Read tags from worker config files
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

$tagsA = Get-WorkerTags $VariantA_WorkerConfig
$tagsB = Get-WorkerTags $VariantB_WorkerConfig

Write-Host "    Variant A: $VariantA_Name (tags: $($tagsA -join ', '))" -ForegroundColor DarkGray
Write-Host "    Variant B: $VariantB_Name (tags: $($tagsB -join ', '))" -ForegroundColor DarkGray

$expBody = @{
    name          = "Multi-Variant Benchmark $(Get-Date -Format 'yyyy-MM-dd HH:mm')"
    test_suite_id = $suiteId
    variants      = @(
        @{
            id          = $variantIdA
            name        = $VariantA_Name
            config_path = $VariantA_Config
            config_hash = ""
            tags        = [array]$tagsA
        },
        @{
            id          = $variantIdB
            name        = $VariantB_Name
            config_path = $VariantB_Config
            config_hash = ""
            tags        = [array]$tagsB
        }
    )
    repetitions = $Repetitions
} | ConvertTo-Json -Depth 5

try {
    $exp = Invoke-RestMethod -Method POST -Uri "$api/benchmark/experiments" `
        -ContentType "application/json" -Body $expBody
    $expId = $exp.data.id
    if (-not $expId) { $expId = $exp.id }
    Write-Ok "Experiment created: $expId (2 variants)"
} catch {
    Cleanup-Workers
    Write-Fail "Failed to create experiment: $_"
}

# ── 5. Run the experiment ───────────────────────────────────
Write-Step "Starting multi-variant experiment run..."
try {
    $runResult = Invoke-RestMethod -Method POST `
        -Uri "$api/benchmark/experiments/$expId/run?timeout_s=$TimeoutS"
    Write-Ok "Experiment started"
} catch {
    Cleanup-Workers
    Write-Fail "Failed to start experiment: $_"
}

# ── 6. Poll for completion ──────────────────────────────────
Write-Step "Waiting for experiment to complete (polling every ${PollIntervalS}s, max ${MaxWaitS}s)..."

$elapsed = 0
$status = "running"
while ($elapsed -lt $MaxWaitS) {
    Start-Sleep -Seconds $PollIntervalS
    $elapsed += $PollIntervalS

    try {
        $check = Invoke-RestMethod "$api/benchmark/experiments/$expId"
        $expData = $check.data
        if (-not $expData) { $expData = $check }
        $status = $expData.status
    } catch {
        Write-Host "    Poll error: $_ (retrying...)" -ForegroundColor Yellow
        continue
    }

    Write-Host "    [$elapsed s] Status: $status" -ForegroundColor DarkGray

    if ($status -eq "complete" -or $status -eq "failed") {
        break
    }
}

if ($status -eq "running") {
    Cleanup-Workers
    Write-Fail "Experiment did not complete within ${MaxWaitS}s. Check orchestrator logs."
}

# ── 7. Fetch and display comparative results ────────────────
Write-Step "Fetching results..."

try {
    $results = Invoke-RestMethod "$api/benchmark/experiments/$expId/results"
    $resData = $results.data
    if (-not $resData) { $resData = $results }
} catch {
    Cleanup-Workers
    Write-Fail "Failed to fetch results: $_"
}

Write-Host "`n" -NoNewline
Write-Host ("=" * 70) -ForegroundColor White
Write-Host "  MULTI-VARIANT BENCHMARK RESULTS" -ForegroundColor White
Write-Host ("=" * 70) -ForegroundColor White

# Summary comparison table
Write-Host "`n  VARIANT COMPARISON:" -ForegroundColor Yellow
Write-Host ("  " + "-" * 66) -ForegroundColor DarkGray

$header = '  {0,-25} {1,12} {2,12} {3,12}' -f 'Metric', $VariantA_Name, $VariantB_Name, 'Delta'
Write-Host $header -ForegroundColor White

Write-Host ("  " + "-" * 66) -ForegroundColor DarkGray

$vrA = $resData.variant_results | Where-Object { $_.variant_name -eq $VariantA_Name }
$vrB = $resData.variant_results | Where-Object { $_.variant_name -eq $VariantB_Name }

function Format-Delta($a, $b) {
    if (-not $a -or -not $b -or $a -eq 0) { return '---' }
    $pct = (($b - $a) / $a) * 100
    $sign = if ($pct -gt 0) { '+' } else { '' }
    $rounded = [math]::Round($pct, 1)
    return ('{0}{1}%' -f $sign, $rounded)
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

    $line = '  {0,-25} {1,12} {2,12} {3,12}' -f $label, $fmtA, $fmtB, $delta
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

    Write-Host ("  " + "-" * 66) -ForegroundColor DarkGray
} else {
    Write-Host '  Could not compare -- missing variant results' -ForegroundColor Yellow
}

# Per-variant detail
foreach ($vr in $resData.variant_results) {
    Write-Host "`n  Variant: $($vr.variant_name)" -ForegroundColor Yellow
    $s = $vr.summary
    Write-Host "    Scenarios: $($s.scenarios_passed) passed / $($s.scenarios_failed) failed / $($s.scenarios_run) total"

    if ($s.avg_e2e_latency_ms) {
        Write-Host "    Avg E2E Latency:  $([math]::Round($s.avg_e2e_latency_ms, 1)) ms"
    }
    if ($s.p50_e2e_latency_ms) {
        Write-Host "    P50 E2E Latency:  $($s.p50_e2e_latency_ms) ms"
    }
    if ($s.p90_e2e_latency_ms) {
        Write-Host "    P90 E2E Latency:  $($s.p90_e2e_latency_ms) ms"
    }
    if ($s.avg_ttft_ms) {
        Write-Host "    Avg TTFT:         $([math]::Round($s.avg_ttft_ms, 1)) ms"
    }
    if ($s.avg_ttfb_ms) {
        Write-Host "    Avg TTFB:         $([math]::Round($s.avg_ttfb_ms, 1)) ms"
    }

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

Write-Host "`n$("=" * 70)" -ForegroundColor White
Write-Host "  Experiment ID: $expId"
Write-Host "  Full JSON:     $api/benchmark/experiments/$expId/results"
Write-Host "  Admin UI:      http://localhost:5173/benchmarks/$expId"
Write-Host ("=" * 70) -ForegroundColor White

# Save raw JSON
$resultsDir = "benchmarks/results"
if (-not (Test-Path $resultsDir)) { New-Item -ItemType Directory -Path $resultsDir -Force | Out-Null }
$outFile = "$resultsDir/benchmark-multi-$(Get-Date -Format 'yyyyMMdd-HHmmss').json"
$resData | ConvertTo-Json -Depth 15 | Set-Content $outFile -Encoding UTF8
Write-Host "`n  Results saved to $outFile" -ForegroundColor Green

# ── 8. Cleanup ──────────────────────────────────────────────
if (-not $SkipWorkers) {
    Write-Step "Stopping worker processes..."
    Cleanup-Workers
    Write-Ok "Workers stopped"
}

Write-Host ''
Write-Host '  Done! Open the Admin UI to see the comparison charts.' -ForegroundColor Green
Write-Host ''
