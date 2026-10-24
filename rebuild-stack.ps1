# rebuild-stack.ps1
# Rebuild + restart the running Voice Pipes stack after code changes,
# WITHOUT re-running the config wizard (unlike launch-stack.ps1, which
# regenerates deploy/docker/config/* and would overwrite manual edits
# such as [[client_tool]] blocks).
#
# Usage:  .\rebuild-stack.ps1            # rebuild worker+orchestrator, restart admin UI
#         .\rebuild-stack.ps1 -SkipAdmin # backend fleet only
#         .\rebuild-stack.ps1 -TakePorts # also stop FOREIGN containers holding our ports

param(
    [switch]$SkipAdmin,
    [switch]$TakePorts
)

# Shared port-conflict preflight (stops our own stale containers; names
# foreign holders instead of failing with a bare "port is already allocated").
. (Join-Path $PSScriptRoot "scripts/preflight-ports.ps1")

$fleetCompose = "voice-pipes/deploy/docker/docker-compose.yml"
$adminCompose = "voice-pipes-admin/docker-compose.yml"

# Compose passes API keys and the VP_* / ADMIN_UI_TOKEN credentials
# through by name from this process's environment, and `up --build`
# recreates containers, so load both .env files exactly as launch-stack.ps1
# does (shell-set values win). Run the launcher once first so the tokens
# exist; the shipped configs refuse to start without them.
foreach ($dotEnvPath in @("voice-pipes/.env", "voice-pipes-admin/.env")) {
    if (-not (Test-Path $dotEnvPath)) { continue }
    foreach ($line in Get-Content $dotEnvPath) {
        if ($line -match '^\s*(#|$)') { continue }
        $key, $value = $line -split '=', 2
        $key = $key.Trim()
        if ($key -and -not (Test-Path "env:$key")) {
            Set-Item -Path "env:$key" -Value $value.Trim().Trim('"')
        }
    }
}
if (-not $env:VP_INTERNAL_TOKEN -or -not $env:ADMIN_UI_TOKEN) {
    Write-Warning "VP_* / ADMIN_UI_TOKEN not found in voice-pipes/.env or voice-pipes-admin/.env; run .\launch-stack.ps1 once to generate them, or the containers will refuse to start."
}

# The wizard's sidecar decisions live in stack-manifest.env (same generic
# contract launch-stack.ps1 consumes): compose profiles, sidecar service
# names, and env vars needed for compose interpolation (e.g. LLAMA_MODEL).
# Re-read it here so a rebuild also revives sidecars that stopped (Docker
# Desktop restart, manual stop) - otherwise the worker comes back up with
# llama-server / pocket-tts DNS names that resolve to nothing.
$manifestPath = "voice-pipes/deploy/docker/config/stack-manifest.env"
$reservedKeys = @('COMPOSE_PROFILES', 'SIDECAR_SERVICES', 'REQUIRES_GPU', 'ENSURE_DIRS')
$manifest = @{}
if (Test-Path $manifestPath) {
    foreach ($line in Get-Content $manifestPath) {
        if ($line -match '^\s*(#|$)') { continue }
        $key, $value = $line -split '=', 2
        $manifest[$key.Trim()] = $value.Trim().Trim('"')
    }
}
$profiles = @(($manifest['COMPOSE_PROFILES'] -split ',') | Where-Object { $_ })
$sidecars = @(($manifest['SIDECAR_SERVICES'] -split ' ') | Where-Object { $_ })
foreach ($key in $manifest.Keys) {
    if ($reservedKeys -contains $key) { continue }
    Set-Item -Path "env:$key" -Value $manifest[$key]
}

# Detect which worker variant is currently running (GPU vs CPU) so the
# rebuild targets the right service and keeps its compose profile.
$running = docker ps --format "{{.Names}}"
if ($running -match "worker-custom-gpu") {
    $worker = "worker-custom-gpu"
    $profiles += "gpu"
} else {
    $worker = "worker-custom"
}
if ($profiles.Count -gt 0) {
    $env:COMPOSE_PROFILES = (($profiles | Select-Object -Unique) -join ',')
}

Write-Host "Rebuilding backend fleet (orchestrator + $worker)..." -ForegroundColor Cyan
if ($sidecars.Count -gt 0) {
    Write-Host "  ensuring sidecars are up: $($sidecars -join ', ')" -ForegroundColor Cyan
}

$fleetServices = @('orchestrator', $worker) + $sidecars
if (-not (Resolve-PortConflicts -ComposeFile $fleetCompose -Services $fleetServices -TakeForeign:$TakePorts)) {
    Write-Error "Port conflicts unresolved - see messages above (or re-run with -TakePorts)."
    Exit 1
}

# `--build` only affects services with a build section; sidecars use pulled
# images, so including them here is a cheap "start if not running" no-op.
# (pocket-tts takes ~2 min after a cold start: it reinstalls its Python env.)
docker compose -f $fleetCompose up --build -d orchestrator $worker $sidecars
if ($LASTEXITCODE -ne 0) {
    Write-Error "Backend fleet rebuild failed."
    Exit 1
}

if (-not $SkipAdmin) {
    # Admin UI: the backend's source dir is bind-mounted (a restart suffices
    # because uvicorn runs without --reload), but the frontend container
    # serves a production bundle baked into its image (nginx) - it must be
    # rebuilt or the playground keeps running the old client. `--build` is
    # a cached no-op when nothing under frontend/ changed.
    Write-Host "Rebuilding Admin UI (backend restart + frontend image build)..." -ForegroundColor Cyan
    # Same preflight as the fleet: a foreign stack (another checkout's admin
    # UI) on 3000/8000 would otherwise fail the restart after the fleet is
    # already rebuilt.
    if (-not (Resolve-PortConflicts -ComposeFile $adminCompose -Services @('backend', 'frontend') -TakeForeign:$TakePorts)) {
        Write-Error "Admin UI port conflicts unresolved - see messages above (or re-run with -TakePorts)."
        Exit 1
    }
    # `up --force-recreate` rather than `restart`: a container that previously
    # failed to start (e.g. port taken at the time) is revived by `restart`
    # WITHOUT its published ports; recreating from the compose file always
    # yields a correctly bound container, and is a plain restart otherwise.
    docker compose -f $adminCompose up -d --build --force-recreate backend frontend
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Admin UI restart failed."
        Exit 1
    }
}

# Trust-check: show what is actually running now.
Write-Host ""
Write-Host "Verification" -ForegroundColor Green
$img = docker image inspect "docker-$worker" --format "{{.Created}}" 2>$null
if ($img) { Write-Host "  $worker image built: $img" }
docker ps --format "table {{.Names}}\t{{.Status}}"
Write-Host ""
Write-Host "Done. Hard-refresh the browser (Ctrl+Shift+R) and reconnect the playground session." -ForegroundColor Green
