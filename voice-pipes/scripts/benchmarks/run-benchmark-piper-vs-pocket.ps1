<#
.SYNOPSIS
    Benchmark: Piper vs Kyutai Pocket-TTS — TTS-only corpus.

.DESCRIPTION
    End-to-end demo script that:
      1. Queries /api/v1/workers to auto-discover config_hash for each worker.
      2. Creates a TtsOnlyCorpus test suite.
      3. Creates a multi-variant experiment with the discovered hashes.
      4. Starts the run and polls until completion.
      5. Prints a per-variant summary table.

    Prerequisites (all must be running before you execute this script):
      - Orchestrator:
          .\target\release\voice-pipes.exe orchestrate
      - Piper worker:
          .\target\release\voice-pipes.exe `
              --config examples/bench-piper.toml serve `
              --worker-config worker-bench-piper.toml
      - Pocket sidecar:
          uvx pocket-tts serve --host 127.0.0.1 --port 8000
      - Pocket worker:
          .\target\release\voice-pipes.exe `
              --config examples/bench-pocket-http.toml serve `
              --worker-config worker-bench-pocket.toml

    Build (one-time):
      cargo build --release -p voice-pipes-cli --features tts-piper,tts-pocket

.PARAMETER OrchestratorUrl
    Base URL of the orchestrator API (default: http://127.0.0.1:9090).

.PARAMETER PocketTtsUrl
    Pocket-TTS sidecar health URL (default: http://127.0.0.1:8000).

.PARAMETER AudioFiles
    WAV scenario fixtures. Defaults cover short and long utterances.

.PARAMETER Repetitions
    Number of repetitions per scenario (default: 1).

.PARAMETER PollIntervalSec
    Seconds between status polls (default: 3).

.PARAMETER TimeoutSec
    Hard cap on waiting for completion (default: 300).

.EXAMPLE
    .\run-benchmark-piper-vs-pocket.ps1

.EXAMPLE
    .\run-benchmark-piper-vs-pocket.ps1 -Repetitions 3 -OrchestratorUrl http://127.0.0.1:9090
#>

[CmdletBinding()]
param(
    [string] $OrchestratorUrl  = 'http://127.0.0.1:9090',
    [int]    $Repetitions      = 1,
    [int]    $PollIntervalSec  = 3,
    [int]    $TimeoutSec       = 300
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
# Orchestrator admin API needs VP_ADMIN_TOKEN (env or voice-pipes/.env).
. (Join-Path $PSScriptRoot "../../../scripts/admin-auth.ps1")
$api = "$OrchestratorUrl/api/v1"

function Invoke-Api {
    param(
        [string] $Method = 'Get',
        [string] $Path,
        [object] $Body = $null
    )
    $params = @{ Method = $Method; Uri = "$api$Path" }
    if ($Body) {
        $params['ContentType'] = 'application/json'
        $params['Body']        = ($Body | ConvertTo-Json -Depth 10 -Compress)
    }
    (Invoke-RestMethod @params).data
}

# ── 1. Discover workers ───────────────────────────────────────────────────────

Write-Host "`n[1/5] Querying registered workers at $OrchestratorUrl ..." -ForegroundColor Cyan
$workers = Invoke-Api -Path '/workers'

if (-not $workers.items -or $workers.items.Count -eq 0) {
    Write-Error "No workers registered. Start both workers before running this script."
}

Write-Host "      Found $($workers.total) worker(s):" -ForegroundColor Gray
$workers.items | Format-Table id, pipeline_name,
    @{L='hash(12)'; E={$_.pipeline_config_hash.Substring(0, [Math]::Min(12, $_.pipeline_config_hash.Length))}},
    tags -AutoSize

# Map from friendly name → (tag pattern, config_path, variant tags)
$knownVariants = @(
    [PSCustomObject]@{ Name = 'Piper';  TagPattern = 'piper';  ConfigPath = 'examples/bench-piper.toml';       VariantTags = @('tts:piper') }
    [PSCustomObject]@{ Name = 'Pocket'; TagPattern = 'pocket'; ConfigPath = 'examples/bench-pocket-http.toml'; VariantTags = @('tts:pocket-http') }
)

$activeVariants = @()
foreach ($kv in $knownVariants) {
    $w = $workers.items |
         Where-Object { ($_.tags | Where-Object { $_ -match $kv.TagPattern }) } |
         Select-Object -First 1
    if ($w) {
        $activeVariants += [PSCustomObject]@{
            Name        = $kv.Name
            Worker      = $w
            ConfigPath  = $kv.ConfigPath
            VariantTags = $kv.VariantTags
        }
        Write-Host "      $($kv.Name) worker: $($w.id)  hash=$($w.pipeline_config_hash.Substring(0,[Math]::Min(12,$w.pipeline_config_hash.Length)))..." -ForegroundColor Gray
    } else {
        Write-Host "      $($kv.Name) worker: not found - variant will be skipped." -ForegroundColor Yellow
    }
}

if ($activeVariants.Count -eq 0) {
    Write-Error "No matching workers found. Start at least one benchmark worker and try again."
}
if ($activeVariants.Count -eq 1) {
    Write-Warning "Only 1 variant available ($($activeVariants[0].Name)). Running single-variant benchmark (no comparison)."
}

# ── 2. Create test suite ──────────────────────────────────────────────────────

Write-Host "`n[2/5] Creating TtsOnlyCorpus test suite..." -ForegroundColor Cyan

$suite = Invoke-Api -Method Post -Path '/benchmark/suites' -Body @{
    name        = 'tts-piper-vs-pocket'
    description = 'Automated Piper vs Pocket TTS comparison'
    scenarios   = @(
        @{
            id    = 'tts-corpus'
            name  = 'TTS corpus'
            input = @{
                type    = 'tts_only_corpus'
                entries = @(
                    @{ id = 'short-plain-01';    text = 'Hello there, how can I help you today?';                                                                                                                    bucket = 'short';  class = 'plain'    }
                    @{ id = 'short-plain-02';    text = 'Please hold while I look that up for you.';                                                                                                                 bucket = 'short';  class = 'plain'    }
                    @{ id = 'short-numerics-01'; text = 'Your order number is 4 8 2 1 9 7.';                                                                                                                         bucket = 'short';  class = 'numerics' }
                    @{ id = 'medium-plain-01';   text = 'The meeting has been rescheduled to Thursday at two thirty in the afternoon. Please update your calendar and confirm your attendance.';                      bucket = 'medium'; class = 'plain'    }
                    @{ id = 'medium-mixed-01';   text = 'Your balance is one hundred and twenty three dollars and forty five cents. The due date is the fifteenth.';                                                  bucket = 'medium'; class = 'mixed'    }
                    @{ id = 'long-plain-01';     text = 'Welcome to our automated assistant. I can help you with account inquiries, order status, billing questions, and general support. What would you like help with today?'; bucket = 'long'; class = 'plain' }
                )
            }
        }
    )
}

Write-Host "      Suite id: $($suite.id)" -ForegroundColor Gray

# ── 3. Create experiment ──────────────────────────────────────────────────────

Write-Host "`n[3/5] Creating experiment..." -ForegroundColor Cyan

$experiment = Invoke-Api -Method Post -Path '/benchmark/experiments' -Body @{
    name          = 'piper-vs-pocket'
    description   = 'Automated Piper vs Pocket TTS comparison'
    test_suite_id = $suite.id
    repetitions   = $Repetitions
    variants      = @(
        $activeVariants | ForEach-Object {
            @{
                id          = $_.Name.ToLower()
                name        = $_.Name
                config_path = $_.ConfigPath
                config_hash = $_.Worker.pipeline_config_hash
                tags        = $_.VariantTags
            }
        }
    )
}

Write-Host "      Experiment id: $($experiment.id)" -ForegroundColor Gray

# ── 4. Run + poll ─────────────────────────────────────────────────────────────

Write-Host "`n[4/5] Starting run..." -ForegroundColor Cyan
Invoke-Api -Method Post -Path "/benchmark/experiments/$($experiment.id)/run" | Out-Null

$deadline = (Get-Date).AddSeconds($TimeoutSec)
$spinner  = @('|', '/', '-', '\')
$tick     = 0

do {
    Start-Sleep -Seconds $PollIntervalSec
    $exp    = Invoke-Api -Path "/benchmark/experiments/$($experiment.id)"
    $status = $exp.status
    Write-Host ("`r      $($spinner[$tick % 4]) $status   ") -NoNewline
    $tick++
    if ((Get-Date) -gt $deadline) {
        Write-Error "Timed out waiting for experiment to complete (${TimeoutSec}s elapsed)."
    }
} while ($status -in @('pending', 'running'))

Write-Host "`r      Status: $status         "
if ($status -eq 'failed') { Write-Warning "Experiment finished with status 'failed'. Check orchestrator logs." }

# ── 5. Summary table ──────────────────────────────────────────────────────────

Write-Host "`n[5/5] Results:" -ForegroundColor Cyan

$results = Invoke-Api -Path "/benchmark/experiments/$($experiment.id)/results"

$rows = $results.variant_results | ForEach-Object {
    $s = $_.summary
    [PSCustomObject]@{
        Variant         = $_.variant_name
        'TTFB ms'       = if ($s.PSObject.Properties['avg_tts_ttfb_ms']      -and $null -ne $s.avg_tts_ttfb_ms)      { [int]$s.avg_tts_ttfb_ms }      else { '-' }
        'Synth ms'      = if ($s.PSObject.Properties['avg_tts_synthesis_ms']  -and $null -ne $s.avg_tts_synthesis_ms)  { [int]$s.avg_tts_synthesis_ms }  else { '-' }
        'Cold ms'       = if ($s.PSObject.Properties['cold_tts_synthesis_ms'] -and $null -ne $s.cold_tts_synthesis_ms) { [int]$s.cold_tts_synthesis_ms } else { '-' }
        'Warm ms'       = if ($s.PSObject.Properties['warm_tts_synthesis_ms'] -and $null -ne $s.warm_tts_synthesis_ms) { [int]$s.warm_tts_synthesis_ms } else { '-' }
        RTF             = if ($s.PSObject.Properties['avg_tts_rtf']           -and $null -ne $s.avg_tts_rtf)           { [math]::Round($s.avg_tts_rtf, 3) } else { '-' }
        'Fail%'         = if ($s.PSObject.Properties['tts_failure_rate']      -and $null -ne $s.tts_failure_rate)      { '{0:P0}' -f $s.tts_failure_rate } else { '-' }
        Scenarios       = $_.scenarios.Count
    }
}

$rows | Format-Table -AutoSize

Write-Host "Admin UI : http://127.0.0.1:5173/benchmarks/$($experiment.id)" -ForegroundColor Green
Write-Host "Full JSON: $api/benchmark/experiments/$($experiment.id)/results" -ForegroundColor Gray
Write-Host '  Done. Open the Admin UI to view comparison charts.' -ForegroundColor Green
Write-Host ''
