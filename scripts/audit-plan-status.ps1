<#
.SYNOPSIS
    Audit PLAN-*.md status headers against shipped code signals.

.DESCRIPTION
    Flags plans whose `**Status:**` header says Draft / Proposed / Planning / In Progress
    but where strong shipped-code signals exist (named Cargo feature, src module, factory
    registration). This catches the failure mode where code lands without the corresponding
    plan-doc PR flipping the status header.

    Read-only; emits a markdown report to stdout and a non-zero exit code if any
    SUSPECT entries are found (so it can be wired into CI / pre-commit).

.EXAMPLE
    pwsh scripts/audit-plan-status.ps1
    pwsh scripts/audit-plan-status.ps1 -Quiet     # exit code only
    pwsh scripts/audit-plan-status.ps1 -Json      # machine-readable
#>
[CmdletBinding()]
param(
    [switch]$Quiet,
    [switch]$Json
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$cargoToml = Join-Path $repoRoot 'voice-pipes\Cargo.toml'

# --- 1. Collect all plan docs and their status headers --------------------
$planFiles = Get-ChildItem -Path $repoRoot -Recurse -Filter 'PLAN-*.md' `
    -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -notmatch '\\archive\\' }

$plans = foreach ($f in $planFiles) {
    $head = Get-Content $f.FullName -TotalCount 30 -ErrorAction SilentlyContinue
    $statusLine = $head | Select-String -Pattern '^\s*\*?\*?Status:?\*?\*?\s*[:\-]?\s*(.+?)\s*$' |
        Select-Object -First 1
    [pscustomobject]@{
        Path   = $f.FullName.Substring($repoRoot.Length + 1).Replace('\','/')
        Name   = $f.BaseName
        Status = if ($statusLine) { $statusLine.Matches[0].Groups[1].Value.Trim('*',' ','`','_') } else { '<missing>' }
    }
}

# --- 2. Build code-signal corpus ------------------------------------------
$cargoFeatures = @()
if (Test-Path $cargoToml) {
    $inFeatures = $false
    foreach ($line in Get-Content $cargoToml) {
        if ($line -match '^\s*\[features\]\s*$') { $inFeatures = $true; continue }
        if ($inFeatures -and $line -match '^\s*\[') { $inFeatures = $false }
        if ($inFeatures -and $line -match '^\s*([a-z][a-z0-9-]+)\s*=') {
            $cargoFeatures += $Matches[1]
        }
    }
}

$srcDirs = @()
foreach ($area in @('stt','tts','text','llm','realtime','audio','vad','pipeline')) {
    $p = Join-Path $repoRoot "voice-pipes\src\$area"
    if (Test-Path $p) {
        $srcDirs += Get-ChildItem $p -Directory -ErrorAction SilentlyContinue |
            ForEach-Object { "$area/$($_.Name)" }
    }
}

# --- 3. Heuristic: extract candidate keywords from each plan's filename ----
function Get-PlanKeywords([string]$name) {
    # PLAN-STT-ASSEMBLYAI -> @('assemblyai'); PLAN-KOKORO-TTS -> @('kokoro')
    # PLAN-OPENAI-LLM     -> @('openai'); PLAN-STREAMING-ASR-PAPER-ADOPTION -> @('streaming','asr','nemotron')
    $stop = @('PLAN','PHASE','SPEC','PLAN2','V2','V3','UI','API','BFF','TTS','STT','LLM','VAD',
              'AUDIO','PIPELINE','PRODUCTION','REFACTOR','SUPPORT','PROVIDER','PROTOCOL',
              'INTEGRATION','BENCHMARK','BENCHMARKING','HISTORY','DETAILS','EDITOR','SYNC',
              'DASHBOARD','OBSERVABILITY','MIGRATION','SESSION','SESSIONS','SESSIONFUL',
              'COMPONENT','BASE','TRAIT','MEMORY','LIVE','GRAPH','PARAM','MULTI','THIN',
              'CLIENT','MULTIFRAMEWORK','MULTIMODAL','LIPSYNC','PER','TURN','METRICS',
              'REGISTRY','PLAYGROUND','HISTORICAL','MODERN','WEB','GUIDANCE','ADOPTION',
              'CROSSCUTTING','POST','PROCESSOR','EXTERNAL','PROVIDERS','PAPER',
              'STREAMING','MULTITURN','BARGEIN','TESTS','GRPC','SPLIT','MODE',
              'INTERRUPTION','RESOLUTION','DIAGNOSTIC','IMPROVEMENTS','REALTIME',
              'SPEECH','TO','DUAL','PATH','AUDIT','ANALYTICS','MULTIPIPELINE','ROUTING',
              'TRANSPORT','GLOBAL','TEXT','WEBTRANSPORT','ERROR','PROPAGATION','TIMING',
              'OPUS','CODEC','REGISTRYUI','SURFACING','NEXT','STEPS','FP32','SPIKE',
              'PARAMS','PATCH','HOSTED','REPORTS','STORE','BACKEND','REPLAY')
    $parts = $name -split '[-_]' | Where-Object { $_ -and ($stop -notcontains $_) }
    # Lowercase, dedupe, drop length<3
    $parts | ForEach-Object { $_.ToLower() } |
        Where-Object { $_.Length -ge 3 } | Select-Object -Unique
}

# --- 4. Score each plan ----------------------------------------------------
$suspectStatuses = @('Draft','Proposed','Planning','In Progress','In-Progress')

$results = foreach ($p in $plans) {
    $isSuspectBucket = $suspectStatuses | Where-Object { $p.Status -match [regex]::Escape($_) }
    if (-not $isSuspectBucket) { continue }

    $keywords = Get-PlanKeywords $p.Name
    if (-not $keywords) { continue }

    $hits = @()
    foreach ($kw in $keywords) {
        $featMatch = $cargoFeatures | Where-Object { $_ -match $kw }
        if ($featMatch) { $hits += "feature=$($featMatch -join ',')" }

        $dirMatch = $srcDirs | Where-Object { $_ -match $kw }
        if ($dirMatch) { $hits += "module=$($dirMatch -join ',')" }
    }

    if ($hits) {
        [pscustomobject]@{
            Plan     = $p.Path
            Status   = $p.Status
            Keywords = ($keywords -join ',')
            Evidence = ($hits | Select-Object -Unique) -join '; '
            Severity = if ($hits.Count -ge 2) { 'HIGH' } else { 'REVIEW' }
        }
    }
}

# --- 5. Render -------------------------------------------------------------
if ($Json) {
    $results | ConvertTo-Json -Depth 4
} elseif (-not $Quiet) {
    Write-Host ""
    Write-Host "=== PLAN-STATUS audit ===" -ForegroundColor Cyan
    Write-Host "Scanned $($plans.Count) plan docs; $($cargoFeatures.Count) Cargo features; $($srcDirs.Count) src modules."
    Write-Host ""
    if (-not $results) {
        Write-Host "OK - no suspect plans found." -ForegroundColor Green
    } else {
        Write-Host "Suspect plans (status says Draft/Proposed/Planning/In-Progress but code signals exist):" -ForegroundColor Yellow
        Write-Host ""
        $results | Sort-Object Severity, Plan | Format-Table -AutoSize -Wrap Severity, Status, Plan, Evidence
        Write-Host ""
        Write-Host "Action: read the plan, verify implementation, then either:"
        Write-Host "  - update the plan's **Status:** header to 'Completed (shipped)' + add a '## 0 Implementation Summary' section"
        Write-Host "  - move the row in docs/internal/PLAN-STATUS.md from its current bucket to Completed"
        Write-Host "  - or, if it's a false positive (keyword collision), no action needed"
    }
}

if ($results) { exit 1 } else { exit 0 }
