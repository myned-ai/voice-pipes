# Seed and run an S2S provider-comparison benchmark (OpenAI Realtime vs
# Gemini Live, plus ElevenLabs Agents when its worker is up) against the
# docker stack's benchmark workers.
#
# Prerequisites:
#   1. Normal stack running (launch-stack.ps1), then add the bench workers.
#      Name the services explicitly — a bare `up -d` would also start
#      worker-a/worker-b, which collide with worker-custom on host port 8085:
#        cd voice-pipes/deploy/docker
#        docker compose up -d --build worker-s2s-openai worker-s2s-gemini
#      (OPENAI_API_KEY and GEMINI_API_KEY must be set in the environment.)
#   2. Orchestrator reachable (default http://localhost:9090).
#   3. Optional third variant: paste a real agent_id into
#      config/pipeline-s2s-elevenlabs.toml (elevenlabs.io -> Agents), set
#      ELEVENLABS_API_KEY, then `docker compose up -d --build
#      worker-s2s-elevenlabs`. The variant is included automatically when
#      that worker is registered; skipped (with a note) otherwise.
#
# The script creates/updates the "s2s-compare" test suite, creates a fresh
# experiment with one variant per provider, runs it, polls to completion and
# prints a comparison table. Results are also browsable in the admin UI under
# /benchmarks.

[CmdletBinding()]
param(
    [string]$OrchestratorUrl = "http://localhost:9090",
    [int]$Repetitions = 3,
    [int]$ScenarioTimeoutS = 120,
    # Create the suite + experiment but don't run (finish in the admin UI).
    [switch]$SeedOnly
)

$ErrorActionPreference = "Stop"
# Orchestrator admin API needs VP_ADMIN_TOKEN (env or voice-pipes/.env).
. (Join-Path $PSScriptRoot "admin-auth.ps1")
$base = "$OrchestratorUrl/api/v1/benchmark"

function Invoke-Api {
    param([string]$Method, [string]$Url, $Body)
    $req = @{ Method = $Method; Uri = $Url; ContentType = "application/json" }
    if ($null -ne $Body) { $req.Body = ($Body | ConvertTo-Json -Depth 20) }
    $resp = Invoke-RestMethod @req
    if ($resp.error) { throw "API error from $($Url): $($resp.error.message)" }
    return $resp.data
}

# ── Preflight: openai + gemini workers required, elevenlabs optional ────────
$workers = (Invoke-RestMethod "$OrchestratorUrl/api/v1/workers").data.items
foreach ($tag in @("s2s-openai", "s2s-gemini")) {
    $match = $workers | Where-Object { $_.tags -contains $tag -and $_.status -eq "active" }
    if (-not $match) {
        Write-Error ("No active worker with tag '$tag'. Start the benchmark workers first:`n" +
            "  cd voice-pipes/deploy/docker; docker compose up -d --build worker-s2s-openai worker-s2s-gemini`n" +
            "  (export the VP_* tokens and API keys from voice-pipes/.env first; the launcher does this,`n" +
            "   or in PowerShell: Get-Content voice-pipes/.env | % { `$k,`$v = `$_ -split '=',2; Set-Item env:`$k `$v })")
        exit 1
    }
}
$hasElevenLabs = [bool]($workers | Where-Object { $_.tags -contains "s2s-elevenlabs" -and $_.status -eq "active" })
if ($hasElevenLabs) {
    Write-Host "Workers OK: s2s-openai + s2s-gemini + s2s-elevenlabs registered." -ForegroundColor Green
} else {
    Write-Host "Workers OK: s2s-openai + s2s-gemini registered." -ForegroundColor Green
    Write-Host ("  (ElevenLabs variant skipped: no active worker tagged 's2s-elevenlabs'. To include it, set a real " +
        "agent_id in config/pipeline-s2s-elevenlabs.toml and start worker-s2s-elevenlabs.)") -ForegroundColor DarkGray
}

# ── Test suite (fixed id → idempotent re-seed) ──────────────────────────────
# Text-input scenarios: both realtime providers accept text turns, which keeps
# the comparison deterministic (no STT variance) while still exercising the
# provider's full generation + TTS path. Latency = text sent → audio out.
$suite = @{
    id          = "s2s-compare"
    name        = "S2S provider comparison"
    description = "Text-driven scenarios for A/B-ing realtime S2S providers: single-turn latency, multi-turn context, barge-in recovery."
    scenarios   = @(
        @{
            id    = "single-turn"
            name  = "Single turn latency"
            input = @{ type = "text_prompt"; text = "In one short sentence, what is the capital of France?" }
        },
        @{
            id    = "multi-turn"
            name  = "Multi-turn context retention"
            input = @{
                type  = "conversation"
                turns = @(
                    @{ id = "t1"; input = @{ type = "text"; text = "My name is Alex and my favourite colour is green. Reply with one short sentence." } },
                    @{ id = "t2"; input = @{ type = "text"; text = "In one short sentence, suggest a flower matching my favourite colour." } },
                    @{ id = "t3"; input = @{ type = "text"; text = "What was my name again? Answer in one short sentence."
                       }; expected = @{ transcript_contains = @("Alex") } }
                )
            }
            # Deterministic only: LLM replies vary, so pass/fail rests on all
            # three turns completing plus the per-turn "Alex" recall check on
            # t3. No latency caps — this suite compares providers, it doesn't
            # gate on speed.
            expected = @{ turns_completed = 3 }
        },
        @{
            id    = "barge-in"
            name  = "Voice barge-in and recovery"
            input = @{
                type      = "barge_in"
                initial   = @{ id = "t1"; input = @{ type = "text"; text = "Tell me a detailed two-minute story about a lighthouse keeper." } }
                # Voice barge-in: the WAV ("Never mind, stop. In one short
                # sentence, what is the capital of Japan?") is streamed as
                # client mic audio 500 ms into playback. Each provider's own
                # server VAD interrupts the story — the production
                # interruption path, and the only one gemini-live/elevenlabs
                # support (they have no client-cancel API). The interrupting
                # utterance doubles as the next user turn, so no follow_up:
                # the runner drains the provider's answer as "interrupt-reply".
                interrupt = @{
                    mode    = @{ kind = "inject_audio"; path = "benchmarks/fixtures/barge-question-16k.wav" }
                    fire_on = @{ kind = "playback_offset"; offset_ms = 500 }
                }
            }
            # pipeline_must_say guards against hollow passes: a cancelled
            # response's stray residue (e.g. "Old...") completes a turn but
            # never contains the answer.
            expected = @{
                must_interrupt          = $true
                follow_up_must_complete = $true
                pipeline_must_say       = @("Tokyo")
            }
        }
    )
}
$null = Invoke-Api POST "$base/suites" $suite
Write-Host "Suite 's2s-compare' seeded ($($suite.scenarios.Count) scenarios)."

# ── Experiment ──────────────────────────────────────────────────────────────
$variants = @(
    @{
        id          = "openai-realtime"
        name        = "OpenAI Realtime"
        config_path = "deploy/docker/config/pipeline-s2s-openai.toml"
        config_hash = ""   # server recomputes at run time (freeze_variant_config)
        tags        = @("s2s-openai")
    },
    @{
        id          = "gemini-live"
        name        = "Gemini Live"
        config_path = "deploy/docker/config/pipeline-s2s-gemini.toml"
        config_hash = ""
        tags        = @("s2s-gemini")
    }
)
if ($hasElevenLabs) {
    # Comparability caveats (see pipeline-s2s-elevenlabs.toml): turn end is
    # idle-inferred so e2e carries ~1.5 s of idle window — compare on TTFB;
    # the persona lives on the ElevenLabs agent unless override_prompt is on.
    $variants += @{
        id          = "elevenlabs-agents"
        name        = "ElevenLabs Agents"
        config_path = "deploy/docker/config/pipeline-s2s-elevenlabs.toml"
        config_hash = ""
        tags        = @("s2s-elevenlabs")
    }
}

$stamp = Get-Date -Format "yyyy-MM-dd HH:mm"
$name = if ($hasElevenLabs) { "OpenAI vs Gemini vs ElevenLabs S2S ($stamp)" } else { "OpenAI vs Gemini S2S ($stamp)" }
$experiment = @{
    name          = $name
    description   = "S2S provider A/B: openai-realtime (gpt-realtime) vs gemini-live (gemini-2.5-flash-native-audio-latest)$(if ($hasElevenLabs) { ' vs elevenlabs (Agents)' })."
    test_suite_id = "s2s-compare"
    repetitions   = $Repetitions
    variants      = $variants
}
$exp = Invoke-Api POST "$base/experiments" $experiment
Write-Host "Experiment created: $($exp.id) ('$($exp.name)')"

if ($SeedOnly) {
    Write-Host "Seed-only mode: run it from the admin UI (/benchmarks) or:"
    Write-Host "  Invoke-RestMethod -Method POST `"$base/experiments/$($exp.id)/run`""
    exit 0
}

# ── Run + poll ──────────────────────────────────────────────────────────────
$null = Invoke-Api POST "$base/experiments/$($exp.id)/run?timeout_s=$ScenarioTimeoutS"
Write-Host "Run started; polling..." -NoNewline

$deadline = (Get-Date).AddMinutes(30)
do {
    Start-Sleep -Seconds 5
    Write-Host "." -NoNewline
    $status = (Invoke-Api GET "$base/experiments/$($exp.id)" $null).status
} while ($status -eq "running" -and (Get-Date) -lt $deadline)
Write-Host ""

if ($status -ne "complete") {
    Write-Warning "Experiment finished with status '$status'. Partial results below; details in the admin UI."
}

# ── Report ──────────────────────────────────────────────────────────────────
$results = Invoke-Api GET "$base/experiments/$($exp.id)/results" $null

$rows = foreach ($vr in $results.variant_results) {
    $s = $vr.summary
    [pscustomobject]@{
        Variant           = $vr.variant_name
        "Run/Pass"        = "$($s.scenarios_run)/$($s.scenarios_passed)"
        "p50 e2e (ms)"    = $s.p50_e2e_latency_ms
        "p90 e2e (ms)"    = $s.p90_e2e_latency_ms
        "avg TTFB (ms)"   = if ($s.avg_ttfb_ms) { [math]::Round($s.avg_ttfb_ms) } else { $null }
        "avg turn (ms)"   = if ($s.avg_turn_latency_ms) { [math]::Round($s.avg_turn_latency_ms) } else { $null }
        "barge-in ok"     = if ($null -ne $s.barge_in_success_rate) { "{0:P0}" -f $s.barge_in_success_rate } else { "-" }
        "barge RTT p50"   = $s.p50_barge_in_rtt_ms
        "recovery p50"    = $s.p50_recovery_ms
        Error             = $vr.error_reason
    }
}
$rows | Format-Table -AutoSize

Write-Host "Full breakdown: admin UI -> /benchmarks/$($exp.id)"
