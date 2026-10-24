<#
.SYNOPSIS
    Benchmark-extraction boundary gate: fails if core engine code references the
    benchmark module outside the small, declared seam.

.DESCRIPTION
    Enforces the invariant that the core
    `voice-pipes` engine must not depend on benchmarking except through the
    documented extension seam, so the whole surface can move to a separate
    `voice-pipes-bench` crate (and later be dropped) without touching core.

    It scans voice-pipes/src/**/*.rs for references to the benchmark module
    (`crate::benchmark`, `voice_pipes::benchmark`, `benchmark_store`,
    `benchmark_routes`) and reports any that live OUTSIDE the allowlist below.

    Allowlist (shrinks as extraction phases land — update it deliberately):
      - src/benchmark/**                       the module itself
      - src/api/orchestrator/benchmark.rs       handlers (will move out later)
      - src/api/orchestrator/mod.rs            the `benchmark_routes` re-export
      - src/commands/orchestrate.rs             composition root (will move elsewhere later)

    The wire hooks (`TtsOnly*`, `InjectAudioFile`) are a SEPARATE, intentional
    seam kept in core; they do not reference the benchmark module and are not
    checked here.

    Read-only. Exits non-zero if any violation is found.

.EXAMPLE
    pwsh scripts/benchmark-boundary-check.ps1
    pwsh scripts/benchmark-boundary-check.ps1 -Quiet   # exit code only
#>
[CmdletBinding()]
param(
    [switch]$Quiet
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$srcRoot = Join-Path $repoRoot 'voice-pipes\src'

# Paths (relative to voice-pipes/src, forward-slashed) permitted to reference
# benchmarking. Now that the benchmark surface lives in the voice-pipes-bench
# crate (composition wired from voice-pipes-cli), the core engine must be fully
# benchmark-free: the allowlist is empty.
$allow = @()

# References that indicate a dependency on benchmarking. Includes the extracted
# crate name — core must not depend on voice-pipes-bench either.
$pattern = 'crate::benchmark|voice_pipes::benchmark|voice_pipes_bench|benchmark_store|benchmark_routes'

function RelSrc([string]$full) {
    $full.Substring($srcRoot.Length + 1).Replace('\', '/')
}

function IsAllowed([string]$rel) {
    foreach ($a in $allow) {
        if ($a.EndsWith('/')) { if ($rel.StartsWith($a)) { return $true } }
        elseif ($rel -eq $a) { return $true }
    }
    return $false
}

$violations = New-Object System.Collections.Generic.List[object]

Get-ChildItem -Path $srcRoot -Recurse -Filter *.rs -File | ForEach-Object {
    $rel = RelSrc $_.FullName
    if (IsAllowed $rel) { return }
    $n = 0
    foreach ($line in [System.IO.File]::ReadAllLines($_.FullName)) {
        $n++
        if ($line -match $pattern) {
            $violations.Add([pscustomobject]@{ File = $rel; Line = $n; Text = $line.Trim() })
        }
    }
}

if (-not $Quiet) {
    if ($violations.Count -eq 0) {
        Write-Output "benchmark-boundary: OK - core engine is benchmark-free (allowlist honored)."
    }
    else {
        Write-Output "benchmark-boundary: $($violations.Count) violation(s) - core references benchmark outside the seam:"
        foreach ($v in $violations) {
            Write-Output ("  voice-pipes/src/{0}:{1}  {2}" -f $v.File, $v.Line, $v.Text)
        }
        Write-Output ""
        Write-Output "Fix: route the dependency through the extension seam, or (if legitimately"
        Write-Output "core) update the allowlist in this script with a rationale."
    }
}

if ($violations.Count -gt 0) { exit 1 }
exit 0
