param(
    [string]$RepoPath = $PSScriptRoot,
    [string]$Profile  = "release",
    [string[]]$CargoArgs = @()
)

$ErrorActionPreference = "Stop"

# ---------------------------------------------------------------------------
# OVERVIEW
# --------
# CUDA 12.8 nvcc has a bug: its embedded host-compiler probe template has no
# #if defined(_WIN32) block, so cl.exe never emits an OS token. nvcc then
# fails with "Host compiler targets unsupported OS."
#
# FIX: nvcc SKIPS the broken probe when it detects VSCMD_VER in the
# environment (indicating a VS Developer environment is already active) AND
# cl.exe is in PATH.  vcvarsall.bat sets VSCMD_VER.
#
# PROBLEM: on machines with long PATH, calling vcvarsall.bat from a
# PowerShell session fails with "input line too long" (cmd.exe 8191-char limit).
#
# SOLUTION: invoke vcvarsall.bat from inside a batch file that first resets
# PATH to just Windows system dirs, so vcvarsall can append MSVC paths
# without hitting the limit.
# ---------------------------------------------------------------------------

# We use the newest installed MSVC toolset (with -allow-unsupported-compiler for nvcc)

# ---------------------------------------------------------------------------
function Find-VcVarsAll {
    $candidates = @(
        "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat",
        "C:\Program Files\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat",
        "C:\Program Files (x86)\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvarsall.bat",
        "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvarsall.bat",
        "C:\Program Files (x86)\Microsoft Visual Studio\2022\Professional\VC\Auxiliary\Build\vcvarsall.bat",
        "C:\Program Files\Microsoft Visual Studio\2022\Professional\VC\Auxiliary\Build\vcvarsall.bat",
        "C:\Program Files (x86)\Microsoft Visual Studio\2022\Enterprise\VC\Auxiliary\Build\vcvarsall.bat",
        "C:\Program Files\Microsoft Visual Studio\2022\Enterprise\VC\Auxiliary\Build\vcvarsall.bat"
    )
    $found = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $found) {
        throw @"
vcvarsall.bat not found.  Install VS 2022 Build Tools:
  winget install Microsoft.VisualStudio.2022.BuildTools
Then add the component: Microsoft.VisualStudio.Component.VC.14.41.17.11.x86.x64
"@
    }
    return $found
}

function Find-MsvcToolsetVersion {
    param([string]$VcVarsAll)
    # Walk up from ...\VC\Auxiliary\Build to VC\Tools\MSVC
    $vcRoot   = Split-Path (Split-Path (Split-Path $VcVarsAll -Parent) -Parent) -Parent
    $msvcRoot = Join-Path $vcRoot "Tools\MSVC"
    if (-not (Test-Path $msvcRoot)) { return $null }

    # Use the newest installed toolset; -allow-unsupported-compiler handles nvcc version checks
    $newest = Get-ChildItem $msvcRoot -Directory |
        Where-Object { try { [version]$_.Name | Out-Null; $true } catch { $false } } |
        Sort-Object { [version]$_.Name } -Descending |
        Select-Object -First 1

    if ($newest) { return $newest.Name }

    throw "No MSVC toolset found under $msvcRoot. Install VS 2022 Build Tools."
}

function New-Junction {
    param([string]$Target, [string]$Link)
    if ($Target -notmatch ' ') { return $Target }
    if (Test-Path $Link) {
        $existing = (Get-Item $Link -Force).Target
        if ($existing -and ($existing.TrimEnd('\') -eq $Target.TrimEnd('\'))) {
            Write-Host "  Junction exists : $Link -> $Target"
            return $Link
        }
        Remove-Item $Link -Recurse -Force
    }
    New-Item -ItemType Junction -Path $Link -Target $Target | Out-Null
    Write-Host "  Junction created: $Link -> $Target"
    return $Link
}

function Find-CudaRoot {
    $nvcc = Get-Command nvcc -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source
    if ($nvcc) { return Split-Path (Split-Path $nvcc -Parent) -Parent }

    $roots = @(
        "C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v12.8",
        "C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v12.7",
        "C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v12.6",
        ($env:CUDA_PATH)
    )
    $found = $roots | Where-Object { $_ -and (Test-Path (Join-Path $_ "bin\nvcc.exe")) } |
        Select-Object -First 1
    if (-not $found) { throw "nvcc not found. Install CUDA Toolkit 12.x." }
    return $found
}

function Find-CargoBin {
    $c = Get-Command cargo -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source
    if ($c) { return Split-Path $c -Parent }
    $default = "$env:USERPROFILE\.cargo\bin"
    if (Test-Path "$default\cargo.exe") { return $default }
    throw "cargo not found. Install Rust from https://rustup.rs"
}

function Find-OrInstall-Protoc {
    param([string]$CacheRoot, [string]$Version = "28.3")

    # 1. Already on PATH?
    $onPath = Get-Command protoc -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source
    if ($onPath) { return $onPath }

    # 2. Honour an existing PROTOC env var if it points at a real binary.
    if ($env:PROTOC -and (Test-Path $env:PROTOC)) { return $env:PROTOC }

    # 3. Previously auto-installed into the local cache?
    $cached = Join-Path $CacheRoot "protoc\bin\protoc.exe"
    if (Test-Path $cached) { return $cached }

    # 4. Download a pinned release into the cache.
    Write-Host "  protoc not found - downloading v$Version ..."
    $dest = Join-Path $CacheRoot "protoc"
    $zip  = Join-Path $CacheRoot "protoc-$Version-win64.zip"
    $url  = "https://github.com/protocolbuffers/protobuf/releases/download/v$Version/protoc-$Version-win64.zip"

    New-Item -ItemType Directory -Force -Path $CacheRoot | Out-Null
    $oldProgress = $ProgressPreference; $ProgressPreference = "SilentlyContinue"
    try {
        Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
    } finally {
        $ProgressPreference = $oldProgress
    }
    if (Test-Path $dest) { Remove-Item $dest -Recurse -Force }
    Expand-Archive -Path $zip -DestinationPath $dest -Force
    Remove-Item $zip -Force

    if (-not (Test-Path $cached)) {
        throw "protoc download succeeded but $cached is missing. Check $url."
    }
    return $cached
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
try {
    Write-Host ""
    Write-Host "=== voice-pipes CUDA build ===" -ForegroundColor Cyan

    # --- Locate toolchain -----------------------------------------------
    Write-Host ""
    Write-Host "--- Locating toolchain ---"

    $vcvarsall  = Find-VcVarsAll
    $toolsetVer = Find-MsvcToolsetVersion -VcVarsAll $vcvarsall
    $cudaRoot   = Find-CudaRoot
    $cargoBin   = Find-CargoBin
    $protoc     = Find-OrInstall-Protoc -CacheRoot (Join-Path $RepoPath ".tools")

    Write-Host "  vcvarsall    : $vcvarsall"
    Write-Host "  MSVC toolset : $toolsetVer"
    Write-Host "  CUDA root    : $cudaRoot"
    Write-Host "  cargo bin    : $cargoBin"
    Write-Host "  protoc       : $protoc"

    # nvcc internally constructs unquoted -I paths from its own location.
    # A space in the CUDA install path breaks that. Use a junction.
    $cudaLink = New-Junction -Target $cudaRoot -Link "C:\vcuda"
    Write-Host "  CUDA (build) : $cudaLink"

    # --- Clean stale candle-kernels artifacts ---------------------------
    Write-Host ""
    Write-Host "--- Cleaning candle-kernels artifacts ---"
    Push-Location $RepoPath
    $buildDir = Join-Path $RepoPath "target\$Profile\build"
    if (Test-Path $buildDir) {
        Get-ChildItem $buildDir -Filter "candle-kernels-*" -Directory -ErrorAction SilentlyContinue |
            ForEach-Object {
                Write-Host "  Removing $($_.FullName)"
                Remove-Item $_.FullName -Recurse -Force
            }
    }
    $prev = $ErrorActionPreference; $ErrorActionPreference = "Continue"
    cargo clean -p candle-kernels 2>&1 | Out-Null
    $ErrorActionPreference = $prev

    # --- Build via fresh batch environment ------------------------------
    #
    # Strategy: the batch file starts with a minimal PATH, then calls
    # vcvarsall.bat so it can extend PATH without hitting the 8191-char
    # limit inherited from a long PowerShell session.
    #
    # vcvarsall.bat sets VSCMD_VER, which tells nvcc "a VS developer
    # environment is active — skip the host-compiler probe".  Without
    # VSCMD_VER, nvcc runs its probe; on CUDA 12.8 Windows the probe
    # template is missing the #if defined(_WIN32) block and nvcc reports
    # "Host compiler targets unsupported OS."
    #
    # -vcvars_ver= pins to the newest installed toolset (pre-built ort-sys
    # requires MSVC 14.42+ STL symbols; -allow-unsupported-compiler lets nvcc
    # accept it).
    # /MD links against the dynamic CRT (required by ONNX Runtime).

    $cargoProfile = if ($Profile -eq "release") { "--release" } else { "--profile $Profile" }
    $extraArgs    = if ($CargoArgs) { $CargoArgs -join " " } else { "" }

    $batch = @"
@echo off
setlocal

rem Minimal PATH: prevents "input line too long" when vcvarsall appends dirs
set "PATH=C:\Windows\System32;C:\Windows;C:\Windows\System32\Wbem"

rem Initialise VS 2022 x64 environment with the CUDA-compatible toolset
call "$vcvarsall" x64 -vcvars_ver=$toolsetVer
if errorlevel 1 (
    echo ERROR: vcvarsall.bat failed >&2
    exit /b 1
)

rem Add CUDA bin and cargo to the now-correct PATH
set "PATH=%PATH%;$cudaLink\bin;$cargoBin"
set "CUDA_PATH=$cudaLink"

rem prost-build needs the protobuf compiler; point it at the located/cached binary
set "PROTOC=$protoc"

rem -allow-unsupported-compiler: lets nvcc work with MSVC > 14.41 (bypasses version check)
rem /MD: match the dynamic CRT used by ONNX Runtime
set "NVCC_PREPEND_FLAGS=-allow-unsupported-compiler -Xcompiler /MD"

echo MSVC toolset : $toolsetVer
echo VSCMD_VER    : %VSCMD_VER%
echo CUDA_PATH    : %CUDA_PATH%
echo.

cd /d "$RepoPath"
cargo build $cargoProfile --bins --all-features $extraArgs
exit /b %ERRORLEVEL%
"@

    $batFile = [IO.Path]::Combine($env:TEMP, "voice_pipes_cuda_build.bat")
    [IO.File]::WriteAllText($batFile, $batch, [Text.Encoding]::ASCII)

    Write-Host ""
    Write-Host "--- Building (Profile: $Profile) ---"
    Write-Host ""

    cmd /c "`"$batFile`""
    $exitCode = $LASTEXITCODE

    if ($exitCode -ne 0) { throw "cargo build failed (exit $exitCode)" }

    Write-Host ""
    Write-Host "=== Build succeeded ===" -ForegroundColor Green

} catch {
    Write-Host ""
    Write-Host "ERROR: $_" -ForegroundColor Red
    exit 1
} finally {
    Pop-Location -ErrorAction SilentlyContinue
}

