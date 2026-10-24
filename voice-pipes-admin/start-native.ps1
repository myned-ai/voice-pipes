# start-native.ps1
# Run the admin console natively (no Docker): backend on http://localhost:8000,
# frontend dev server on http://localhost:5173, against an orchestrator on
# http://localhost:9090 (the Docker launcher's, or a native one).
#
# It installs only what CI tests: Python 3.12 with every package from the
# hash-checked lock (backend/requirements.txt) in backend/.venv, and the
# frontend's package-lock.json on Node 22 or 24. A re-run reinstalls only
# when a lock file changed.
#
# -BackendOnly / -FrontendOnly: run one half.  -Reinstall: rebuild the venv
# and node_modules from scratch.

param(
    [switch]$BackendOnly,
    [switch]$FrontendOnly,
    [switch]$Reinstall
)

# Not 'Stop': Windows PowerShell 5.1 would turn a native command's redirected
# stderr into a terminating error. Each native step checks $LASTEXITCODE.
$ErrorActionPreference = 'Continue'
$OutputEncoding = [System.Text.Encoding]::UTF8
$admin = $PSScriptRoot
$repo = Split-Path $admin -Parent
$backend = Join-Path $admin 'backend'
$frontend = Join-Path $admin 'frontend'

function Fail([string]$message) {
    Write-Host "error: $message" -ForegroundColor Red
    exit 1
}

function FileHash([string]$path) { (Get-FileHash -Algorithm SHA256 $path).Hash }

# KEY=VALUE lines of an env file, quotes stripped, comments skipped.
function Read-EnvFile([string]$path) {
    $values = @{}
    if (Test-Path $path) {
        foreach ($line in Get-Content $path) {
            if ($line -match '^\s*([A-Za-z_][A-Za-z0-9_]*)=(.*)$') {
                $values[$Matches[1]] = $Matches[2].Trim().Trim('"').Trim("'")
            }
        }
    }
    $values
}

function Set-IfUnset([string]$name, [string]$value) {
    if ($value -and -not [Environment]::GetEnvironmentVariable($name)) {
        [Environment]::SetEnvironmentVariable($name, $value)
    }
}

# ── Settings ───────────────────────────────────────────────────────────
# voice-pipes-admin/.env holds ADMIN_UI_TOKEN (generated once, as the Docker
# launcher does); the engine tokens come from voice-pipes/.env, where the
# launcher keeps them. Values already set in this shell win.
$adminEnv = Join-Path $admin '.env'
if (-not (Read-EnvFile $adminEnv)['ADMIN_UI_TOKEN']) {
    $bytes = New-Object byte[] 32
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
    Add-Content -Path $adminEnv -Value ("ADMIN_UI_TOKEN=" + (($bytes | ForEach-Object { $_.ToString('x2') }) -join ''))
    Write-Host "Generated ADMIN_UI_TOKEN into $adminEnv" -ForegroundColor DarkGray
}
$adminValues = Read-EnvFile $adminEnv
foreach ($key in $adminValues.Keys) { Set-IfUnset $key $adminValues[$key] }
$engineValues = Read-EnvFile (Join-Path $repo 'voice-pipes/.env')
foreach ($key in 'VP_ADMIN_TOKEN', 'VP_CLIENT_TOKEN') { Set-IfUnset $key $engineValues[$key] }
# A native backend reaches the orchestrator and the captures on this host,
# not under the container names the image defaults to.
Set-IfUnset 'ORCHESTRATOR_URL' 'http://localhost:9090'
Set-IfUnset 'CAPTURES_DIR' (Join-Path $repo 'voice-pipes/captures')

# ── Backend: Python 3.12 and the locked packages ───────────────────────
$venvPython = Join-Path $backend '.venv/Scripts/python.exe'
if (-not $FrontendOnly) {
    $python = $null
    if (Get-Command py -ErrorAction SilentlyContinue) {
        $found = & py -3.12 -c "import sys; print(sys.executable)" 2>$null
        if ($LASTEXITCODE -eq 0) { $python = $found.Trim() }
    }
    if (-not $python -and (Get-Command python3.12 -ErrorAction SilentlyContinue)) {
        $python = (Get-Command python3.12).Source
    }
    if (-not $python) {
        Fail "Python 3.12 not found. Install it (winget install Python.Python.3.12) and run this again. The backend is tested on 3.12 only."
    }

    $venv = Join-Path $backend '.venv'
    if ((Test-Path $venv) -and -not $Reinstall) {
        $venvVersion = if (Test-Path $venvPython) { & $venvPython -c "import sys; print('%d.%d' % sys.version_info[:2])" 2>$null } else { '' }
        if ($venvVersion -ne '3.12') {
            Write-Host "backend/.venv is Python '$venvVersion', not 3.12: recreating it." -ForegroundColor Yellow
            $Reinstall = $true
        }
    }
    if ($Reinstall -and (Test-Path $venv)) { Remove-Item -Recurse -Force $venv }
    if (-not (Test-Path $venvPython)) {
        Write-Host "Creating backend/.venv with $python" -ForegroundColor Gray
        & $python -m venv $venv
        if ($LASTEXITCODE -ne 0) { Fail "could not create backend/.venv" }
    }
    $lock = Join-Path $backend 'requirements.txt'
    $marker = Join-Path $venv '.installed-lock-sha256'
    if (-not (Test-Path $marker) -or (Get-Content $marker -Raw).Trim() -ne (FileHash $lock)) {
        # An environment made by uv has no pip; Python's own ensurepip adds it.
        & $venvPython -m pip --version *> $null
        if ($LASTEXITCODE -ne 0) {
            & $venvPython -m ensurepip --upgrade *> $null
            if ($LASTEXITCODE -ne 0) { Fail "backend/.venv has no pip and ensurepip failed; run again with -Reinstall" }
        }
        Write-Host "Installing the backend's locked packages..." -ForegroundColor Gray
        & $venvPython -m pip install --disable-pip-version-check --require-hashes -r $lock
        if ($LASTEXITCODE -ne 0) { Fail "pip install from backend/requirements.txt failed (see above)" }
        Set-Content -Path $marker -Value (FileHash $lock)
    }
}

# ── Frontend: Node 22 or 24 and the locked packages ────────────────────
if (-not $BackendOnly) {
    if (-not (Get-Command node -ErrorAction SilentlyContinue)) {
        Fail "Node.js not found. Install Node 24 LTS (winget install OpenJS.NodeJS.LTS) and run this again."
    }
    $nodeVersion = (& node --version).Trim().TrimStart('v')
    $major, $minor = $nodeVersion.Split('.')[0..1] | ForEach-Object { [int]$_ }
    if (-not (($major -eq 22 -and $minor -ge 13) -or $major -eq 24)) {
        Fail "Node $nodeVersion is not one the console is tested on (22.13+ or 24). Install Node 24 LTS, or switch with a version manager (fnm use 24)."
    }
    $lock = Join-Path $frontend 'package-lock.json'
    $marker = Join-Path $frontend 'node_modules/.installed-lock-sha256'
    if ($Reinstall -or -not (Test-Path $marker) -or (Get-Content $marker -Raw).Trim() -ne (FileHash $lock)) {
        Write-Host "Installing the frontend's locked packages (npm ci)..." -ForegroundColor Gray
        Push-Location $frontend
        try { & npm ci } finally { Pop-Location }
        if ($LASTEXITCODE -ne 0) { Fail "npm ci failed (see above)" }
        Set-Content -Path $marker -Value (FileHash $lock)
    }
}

# ── Run ────────────────────────────────────────────────────────────────
# The Docker admin console publishes 8000 too; say so instead of timing out.
$ports = @()
if (-not $FrontendOnly) { $ports += 8000 }
if (-not $BackendOnly) { $ports += 5173 }
foreach ($port in $ports) {
    $holder = Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($holder) {
        $name = (Get-Process -Id $holder.OwningProcess -ErrorAction SilentlyContinue).ProcessName
        Fail "port $port is in use by '$name' (pid $($holder.OwningProcess)). If it is the Docker admin console: docker compose -f voice-pipes-admin/docker-compose.yml down"
    }
}

$uvicornArgs = @('-m', 'uvicorn', 'app.main:app', '--host', '127.0.0.1', '--port', '8000')
Write-Host ""
Write-Host "Orchestrator: $env:ORCHESTRATOR_URL   Captures: $env:CAPTURES_DIR" -ForegroundColor DarkGray
Write-Host "Sign-in token: ADMIN_UI_TOKEN in $adminEnv" -ForegroundColor DarkGray

if ($BackendOnly) {
    Write-Host "Backend on http://localhost:8000 (Ctrl+C stops it)" -ForegroundColor Green
    Push-Location $backend
    try { & $venvPython @uvicornArgs } finally { Pop-Location }
    exit $LASTEXITCODE
}
if ($FrontendOnly) {
    Write-Host "Console on http://localhost:5173 (Ctrl+C stops it; the backend must be running)" -ForegroundColor Green
    Push-Location $frontend
    try { & npm run dev -- --port 5173 --strictPort } finally { Pop-Location }
    exit $LASTEXITCODE
}

$server = Start-Process -FilePath $venvPython -ArgumentList $uvicornArgs -WorkingDirectory $backend -NoNewWindow -PassThru
try {
    $ready = $false
    for ($i = 0; $i -lt 60 -and -not $server.HasExited; $i++) {
        try { Invoke-WebRequest -UseBasicParsing -TimeoutSec 2 http://127.0.0.1:8000/livez | Out-Null; $ready = $true; break } catch { Start-Sleep -Milliseconds 500 }
    }
    if (-not $ready) { Fail "the backend did not come up on port 8000 (see its log above)" }
    Write-Host "Console on http://localhost:5173 (backend on :8000). Ctrl+C stops both." -ForegroundColor Green
    Push-Location $frontend
    try { & npm run dev -- --port 5173 --strictPort } finally { Pop-Location }
} finally {
    if (-not $server.HasExited) { Stop-Process -Id $server.Id -Force }
}
