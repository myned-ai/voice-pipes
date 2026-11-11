<#
.SYNOPSIS
    Documentation drift gate: link resolution, PLAN-STATUS completeness, and
    source-path reference checks across all non-vendored docs.

.DESCRIPTION
    Sibling to audit-plan-status.ps1. Catches the doc-rot classes found in the
    2026-07 audit (see docs/DOC-AUDIT-2026-07.md):

      1. BROKEN LINK           — a relative markdown link [..](path) that resolves
                                 neither relative to the doc nor to the repo root.
                                 ERROR for .md targets (doc navigation); WARN for
                                 code-file pointers (informational, drift more).
      2. PLAN-STATUS (error)   — a PLAN-*.md with no `**Status:**` header, or one
                                 that is not listed in docs/PLAN-STATUS.md.
      3. SRC PATH (warn)       — a backticked `src/...` path reference in a spec
                                 that does not exist on disk (dir-vs-file drift).
                                 Warn-only: illustrative paths produce false hits.

    Read-only. Emits a report to stdout and exits non-zero if any ERROR-level
    findings exist (warnings do not fail CI). Wire into pre-commit / CI alongside
    audit-plan-status.ps1.

.EXAMPLE
    pwsh scripts/docs-check.ps1
    pwsh scripts/docs-check.ps1 -Quiet      # exit code only
    pwsh scripts/docs-check.ps1 -Json       # machine-readable findings
    pwsh scripts/docs-check.ps1 -Strict     # warnings also fail CI
#>
[CmdletBinding()]
param(
    [switch]$Quiet,
    [switch]$Json,
    [switch]$Strict
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot

# Directories that are vendored, generated, or intentionally historical.
# Separator class, not a literal backslash: this runs on Windows locally and
# on Linux in CI, and a Windows-only pattern silently excludes nothing there —
# which is how the archive came to be scanned in CI but not on a developer's
# machine, and how a clean local run stopped meaning a clean gate.
$excludeRe = '[\\/](node_modules|target|\.git|\.cache|VibeVoice|vendor|models[\\/]\.cache|archive)[\\/]'

function Rel([string]$full) { $full.Substring($repoRoot.Length + 1).Replace('\','/') }

# The heading anchors of a markdown file, slugged the way GitHub does it
# (lower case, punctuation other than hyphens dropped, spaces to hyphens;
# a repeated heading gets -1, -2, ...). Headings inside fenced code are not
# headings. Cached per file: one doc is the target of many links.
$script:anchorCache = @{}
function Get-Anchors([string]$path) {
    if ($script:anchorCache.ContainsKey($path)) { return $script:anchorCache[$path] }
    $seen = @{}
    $anchors = @{}
    $inFence = $false
    foreach ($line in (Get-Content -LiteralPath $path -Encoding utf8 -ErrorAction SilentlyContinue)) {
        if ($line -match '^\s*(```|~~~)') { $inFence = -not $inFence; continue }
        if ($inFence) { continue }
        if ($line -match '^#{1,6}\s+(.+?)\s*#*\s*$') {
            $slug = $Matches[1].ToLowerInvariant() -replace '[^\p{L}\p{N}\s_-]', '' -replace '\s', '-'
            if ($seen.ContainsKey($slug)) { $seen[$slug]++; $slug = "$slug-$($seen[$slug])" } else { $seen[$slug] = 0 }
            $anchors[$slug] = $true
        }
    }
    $script:anchorCache[$path] = $anchors
    return $anchors
}
function Test-Anchor([string]$path, [string]$anchor) {
    $anchors = Get-Anchors $path
    return $anchors.ContainsKey($anchor.ToLowerInvariant())
}

$findings = New-Object System.Collections.Generic.List[object]
function Add-Finding($level, $check, $file, $line, $message) {
    $findings.Add([pscustomobject]@{
        Level = $level; Check = $check; File = $file; Line = $line; Message = $message
    })
}

# --- Collect docs in scope -------------------------------------------------
$docs = Get-ChildItem -Path $repoRoot -Recurse -Filter '*.md' -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -notmatch $excludeRe }

# ===========================================================================
# Check 1 — markdown link resolution
# ===========================================================================
$linkRe = [regex]'\]\(([^)]+)\)'
foreach ($doc in $docs) {
    $dir = $doc.DirectoryName
    $lineNo = 0
    foreach ($text in (Get-Content $doc.FullName -ErrorAction SilentlyContinue)) {
        $lineNo++
        foreach ($m in $linkRe.Matches($text)) {
            $target = $m.Groups[1].Value.Trim()
            # Strip an optional "title" after a space; keep the #anchor aside.
            $target = ($target -split '\s+')[0]
            $parts = $target -split '#', 2
            $target = $parts[0]
            $anchor = if ($parts.Count -gt 1) { $parts[1] } else { '' }
            if (-not $target) { continue }                          # pure #anchor
            if ($target -match '^(https?:|mailto:|tel:|data:|//)') { continue }
            if ($target -match '^[a-z][a-z0-9+.-]*://') { continue }
            # Resolve relative to the doc first, then fall back to repo root
            # (docs use both doc-relative and repo-root-relative path conventions).
            $relResolved  = [System.IO.Path]::GetFullPath((Join-Path $dir $target))
            $rootResolved = [System.IO.Path]::GetFullPath((Join-Path $repoRoot $target))
            $resolved = if (Test-Path -LiteralPath $relResolved) { $relResolved } elseif (Test-Path -LiteralPath $rootResolved) { $rootResolved } else { $null }
            if ($resolved) {
                # A `file.md#anchor` link must name a heading of that file:
                # a link to a section that was renamed scrolls nowhere.
                # `#L24` is a line reference, not a heading.
                if ($anchor -and $resolved -match '\.md$' -and $anchor -notmatch '^L\d+(-L\d+)?$') {
                    if (-not (Test-Anchor $resolved $anchor)) {
                        Add-Finding 'ERROR' 'broken-anchor' (Rel $doc.FullName) $lineNo "-> $target#$anchor (no such heading)"
                    }
                }
                continue
            }
            # .md targets are navigation (must resolve); other targets are code
            # pointers that drift and are informational, so warn rather than fail.
            $level = if ($target -match '\.md$') { 'ERROR' } else { 'WARN' }
            Add-Finding $level 'broken-link' (Rel $doc.FullName) $lineNo "-> $target"
        }
    }
}

# ===========================================================================
# Check 2 — PLAN-STATUS completeness
# ===========================================================================
$planStatusPath = Join-Path $repoRoot 'docs\internal\PLAN-STATUS.md'
$planStatusText = if (Test-Path $planStatusPath) { Get-Content $planStatusPath -Raw } else { '' }

$planFiles = $docs | Where-Object { $_.Name -like 'PLAN-*.md' }
foreach ($pf in $planFiles) {
    $head = Get-Content $pf.FullName -TotalCount 30 -ErrorAction SilentlyContinue
    $hasStatus = $head | Select-String -Pattern '^\s*[>#*]*\s*\*{0,2}Status' -Quiet
    if (-not $hasStatus) {
        Add-Finding 'ERROR' 'plan-no-status' (Rel $pf.FullName) 1 'no **Status:** header in first 30 lines'
    }
    # PLAN-STATUS.md is the index itself — it need not list itself.
    if ($pf.Name -ne 'PLAN-STATUS.md' -and $planStatusText -and ($planStatusText -notmatch [regex]::Escape($pf.Name))) {
        Add-Finding 'ERROR' 'plan-unlisted' (Rel $pf.FullName) 1 'not referenced in docs/internal/PLAN-STATUS.md'
    }
}

# ===========================================================================
# Check 3 — backticked src/... path references (warn-only)
# Limited to engine/admin spec trees where paths are meant to be concrete.
# ===========================================================================
# The snapshot spec trees moved to docs/internal/specs-snapshot/{engine,admin} (WS-A of
# PLAN-OSS-DOCS-AND-RELEASE-PACKAGING); engine docs stay in voice-pipes/docs.
$pathDocRoots = @('docs\internal\specs-snapshot\engine', 'voice-pipes\docs', 'docs\internal\specs-snapshot\admin')
$srcPathRe = [regex]'`(src/[A-Za-z0-9_./-]+)`'
foreach ($doc in $docs) {
    $relDoc = Rel $doc.FullName
    if (-not ($pathDocRoots | Where-Object { $relDoc -like "$($_.Replace('\','/'))/*" })) { continue }
    # Resolve `src/...` relative to the nearest crate root (voice-pipes or voice-pipes-admin/backend).
    $crateRoot = if ($relDoc -like 'docs/internal/specs-snapshot/admin/*') { Join-Path $repoRoot 'voice-pipes-admin\backend' }
                 else { Join-Path $repoRoot 'voice-pipes' }
    $lineNo = 0
    foreach ($text in (Get-Content $doc.FullName -ErrorAction SilentlyContinue)) {
        $lineNo++
        foreach ($m in $srcPathRe.Matches($text)) {
            $p = $m.Groups[1].Value.TrimEnd('/')
            # `Join-Path` and `Test-Path` both take forward slashes on either
            # platform; rewriting them to backslashes made every one of these
            # a literal filename on Linux, so CI reported src-path warnings
            # for files that were plainly there.
            $resolved = Join-Path $crateRoot $p
            if (-not (Test-Path -LiteralPath $resolved)) {
                Add-Finding 'WARN' 'src-path' $relDoc $lineNo "'$($m.Groups[1].Value)' not found under $(Rel $crateRoot)"
            }
        }
    }
}

# ===========================================================================
# Check 4 — front matter on public docs
# Every page under docs/ (outside docs/internal/) must declare a Status line
# in its first 30 lines (docs/contributing/docs-style.md).
# ===========================================================================
foreach ($doc in $docs) {
    $relDoc = Rel $doc.FullName
    if (-not ($relDoc -like 'docs/*')) { continue }
    if ($relDoc -like 'docs/internal/*') { continue }
    if ($doc.Name -eq 'llms.txt' -or $doc.Name -eq 'SUMMARY.md') { continue }
    $head = Get-Content $doc.FullName -TotalCount 30 -ErrorAction SilentlyContinue
    $hasStatus = $head | Select-String -Pattern '^\s*[>#*]*\s*\*{0,2}Status' -Quiet
    if (-not $hasStatus) {
        Add-Finding 'ERROR' 'front-matter' $relDoc 1 'public doc has no **Status:** line in its first 30 lines'
    }
}

# ===========================================================================
# Check 6 — every public page is in the mdBook sidebar (docs/SUMMARY.md)
# ===========================================================================
$summaryPath = Join-Path $repoRoot 'docs\SUMMARY.md'
if (Test-Path $summaryPath) {
    $summaryText = Get-Content $summaryPath -Raw
    foreach ($doc in $docs) {
        $relDoc = Rel $doc.FullName
        if (-not ($relDoc -like 'docs/*') -or ($relDoc -like 'docs/internal/*')) { continue }
        if ($doc.Name -eq 'SUMMARY.md') { continue }
        $inDocs = $relDoc.Substring('docs/'.Length)
        if ($summaryText -notmatch [regex]::Escape("($inDocs)")) {
            Add-Finding 'ERROR' 'summary' $relDoc 1 'not listed in docs/SUMMARY.md (mdBook sidebar)'
        }
    }
}

# ===========================================================================
# Check 5 — public docs must not link into docs/internal/
# The assembled public tree does not contain docs/internal/.
# ===========================================================================
foreach ($doc in $docs) {
    $relDoc = Rel $doc.FullName
    if ($relDoc -like 'docs/internal/*') { continue }
    $dir = $doc.DirectoryName
    $lineNo = 0
    foreach ($text in (Get-Content $doc.FullName -ErrorAction SilentlyContinue)) {
        $lineNo++
        foreach ($m in $linkRe.Matches($text)) {
            $target = (($m.Groups[1].Value.Trim() -split '\s+')[0] -split '#')[0]
            if (-not $target -or $target -match '^[a-z][a-z0-9+.-]*:' -or $target -match '^//') { continue }
            foreach ($base in @($dir, $repoRoot)) {
                $resolved = [System.IO.Path]::GetFullPath((Join-Path $base $target))
                # Normalise before matching: on Linux the resolved path has
                # forward slashes, so a backslash pattern here meant this
                # check quietly never fired in CI.
                $resolvedNorm = $resolved -replace '\\', '/'
                if ((Test-Path -LiteralPath $resolved) -and ($resolvedNorm -like "*/docs/internal/*")) {
                    Add-Finding 'ERROR' 'internal-link' $relDoc $lineNo "-> $target (docs/internal is not published)"
                    break
                }
            }
        }
    }
}

# ===========================================================================
# Render
# ===========================================================================
$errors = @($findings | Where-Object { $_.Level -eq 'ERROR' })
$warns  = @($findings | Where-Object { $_.Level -eq 'WARN' })

if ($Json) {
    $findings | ConvertTo-Json -Depth 4
} elseif (-not $Quiet) {
    Write-Host ""
    Write-Host "=== docs-check ===" -ForegroundColor Cyan
    Write-Host "Scanned $($docs.Count) docs. $($errors.Count) error(s), $($warns.Count) warning(s)."
    Write-Host ""
    if ($findings.Count -eq 0) {
        Write-Host "OK - no findings." -ForegroundColor Green
    } else {
        foreach ($f in ($findings | Sort-Object Level, Check, File, Line)) {
            $color = if ($f.Level -eq 'ERROR') { 'Red' } else { 'Yellow' }
            Write-Host ("[{0,-5}] {1,-14} {2}:{3}  {4}" -f $f.Level, $f.Check, $f.File, $f.Line, $f.Message) -ForegroundColor $color
        }
        Write-Host ""
        Write-Host "broken-link / broken-anchor / plan-* are errors (fail CI). src-path is warn-only (-Strict to enforce)."
    }
}

$failCount = if ($Strict) { $findings.Count } else { $errors.Count }
if ($failCount -gt 0) { exit 1 } else { exit 0 }
