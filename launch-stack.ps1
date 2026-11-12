# launch-stack.ps1
# Unified Docker-based fleet configurator & launcher for Voice Pipes.
#
# -TakePorts: also stop FOREIGN containers (other compose projects) that hold
#             ports this stack needs; without it they are named and the launch
#             aborts with instructions.
# -SkipWizard: start the stack on the configuration already under
#             voice-pipes/deploy/docker/config/ (from an earlier run, or from
#             `voice-pipes config-wizard` run natively) instead of asking again.

param(
    [switch]$TakePorts,
    [switch]$SkipWizard,
    [switch]$Cpu,
    [switch]$NoGpu
)

# Enable UTF-8 encoding
$OutputEncoding = [System.Text.Encoding]::UTF8

# Shared port-conflict preflight (stops our own stale containers; names
# foreign holders instead of failing with a bare "port is already allocated").
. (Join-Path $PSScriptRoot "scripts/preflight-ports.ps1")

Write-Host "=======================================================" -ForegroundColor Cyan
Write-Host "      VOICE PIPES FLEET LAUNCHER (DOCKER)              " -ForegroundColor Cyan
Write-Host "=======================================================" -ForegroundColor Cyan

# 1. Verify Docker is running
Write-Host "Checking if Docker is running..." -ForegroundColor Gray
docker info > $null 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-Error "Error: Docker is not running or not accessible. Please start Docker Desktop and try again."
    Exit 1
}
Write-Host "OK: Docker is active." -ForegroundColor Green

# 2. Probe GPU Support
if ($Cpu -or $NoGpu -or $env:VP_DEVICE -eq "cpu" -or $env:VP_FORCE_CPU) {
    $gpu_supported = $false
    Write-Host "Running in CPU-only mode (-Cpu flag / VP_DEVICE=cpu)." -ForegroundColor Yellow
} else {
    Write-Host "Probing for NVIDIA GPU container pass-through capability..." -ForegroundColor Gray
    $gpu_supported = $false
    docker run --rm --gpus all nvidia/cuda:12.4.1-base-ubuntu22.04 nvidia-smi > $null 2>&1
    if ($LASTEXITCODE -eq 0) {
        $gpu_supported = $true
        Write-Host "NVIDIA GPU container support detected!" -ForegroundColor Green

        # Check compute capability for Blackwell (RTX 50-series, sm_120):
        # ONNX Runtime 1.20 bundled in the CUDA image has no precompiled kernels for sm_120.
        # Default VP_ORT_EP=cpu so ONNX models (Piper, Nemotron, etc.) run on CPU,
        # while llama-server continues to run on GPU.
        $computeCap = ""
        try {
            $computeCap = (nvidia-smi --query-gpu=compute_cap --format=csv,noheader 2>$null).Trim()
        } catch {}
        if ($computeCap -and [double]::TryParse($computeCap, [ref]$null) -and [double]$computeCap -ge 12.0) {
            if (-not $env:VP_ORT_EP) {
                $env:VP_ORT_EP = "cpu"
                Write-Host "Detected NVIDIA Blackwell GPU (compute capability $computeCap)." -ForegroundColor Yellow
                Write-Host "   Setting VP_ORT_EP=cpu for ONNX models (llama-server continues on GPU)." -ForegroundColor Yellow
            }
        }
    } else {
        Write-Host "NVIDIA GPU pass-through is not available on this Docker engine." -ForegroundColor Yellow
        Write-Host "   Defaulting to CPU-only execution." -ForegroundColor Yellow
    }
}

if ($SkipWizard) {
    if (-not (Test-Path "voice-pipes/deploy/docker/config/pipeline-custom.toml")) {
        Write-Error "Error: -SkipWizard needs an existing configuration (voice-pipes/deploy/docker/config/pipeline-custom.toml); run without it first."
        Exit 1
    }
    Write-Host "Skipping the wizard; using the existing configuration." -ForegroundColor Gray
} else {
# 3. Build the builder container for the config wizard
Write-Host "Preparing configuration wizard container..." -ForegroundColor Gray
docker build -t voice-pipes-builder -f voice-pipes/Dockerfile .
if ($LASTEXITCODE -ne 0) {
    Write-Error "Error: Failed to build builder container."
    Exit 1
}

# 3b. Resolve a HuggingFace access token (needed only for gated repos, e.g. Gemma 3).
#     The wizard downloads models *inside the container*, so the host's
#     `huggingface-cli login` token is not visible to it - we must pass the token
#     in via the HF_TOKEN environment variable.
Write-Host "Checking for HuggingFace access token (needed for gated models like Gemma 3)..." -ForegroundColor Gray
if (-not $env:HF_TOKEN) {
    # Fall back to the token cached by `huggingface-cli login`, if present.
    $hfTokenFile = Join-Path $env:USERPROFILE ".cache\huggingface\token"
    if (Test-Path $hfTokenFile) {
        $env:HF_TOKEN = (Get-Content $hfTokenFile -Raw).Trim()
        Write-Host "Using HuggingFace token from $hfTokenFile" -ForegroundColor Green
    }
}
if (-not $env:HF_TOKEN) {
    Write-Host "No HF_TOKEN found." -ForegroundColor Yellow
    Write-Host "  Public models (SmolLM2, Parakeet, Piper, Moonshine) download without one." -ForegroundColor DarkGray
    Write-Host "  Gated models (e.g. Gemma 3) need a token: accept the license on the model page," -ForegroundColor DarkGray
    Write-Host "  then create a read token at https://huggingface.co/settings/tokens" -ForegroundColor DarkGray
    $secureToken = Read-Host "Paste HuggingFace token (or press Enter to skip)" -AsSecureString
    $plainToken  = [System.Net.NetworkCredential]::new("", $secureToken).Password
    if ($plainToken) { $env:HF_TOKEN = $plainToken }
}
if ($env:HF_TOKEN) {
    Write-Host "OK: HuggingFace token configured." -ForegroundColor Green
} else {
    Write-Host "Continuing without a token (gated models will fail to download)." -ForegroundColor Yellow
}

# Build the env-passthrough args. Passing the var *by name* (no value) keeps the
# secret out of the process arg list / `docker inspect`.
$hfEnvArgs = @()
if ($env:HF_TOKEN) { $hfEnvArgs = @("-e", "HF_TOKEN") }

# 4. Run the interactive configurator wizard
Write-Host "Launching interactive configuration wizard..." -ForegroundColor Gray
# VP_WIZARD_FROM_LAUNCHER tells the wizard this script starts the stack
# next, so it prints no next step of its own.
docker run -it --rm @hfEnvArgs -e VP_WIZARD_FROM_LAUNCHER=1 -v "${PWD}/voice-pipes:/app" voice-pipes-builder config-wizard
if ($LASTEXITCODE -ne 0) {
    Write-Error "Error: Configuration wizard exited with an error."
    Exit 1
}
}

# 5. Start the backend services (Orchestrator + Worker)
Write-Host "Starting Voice Pipes backend fleet..." -ForegroundColor Gray

# The wizard hands off its sidecar decisions in one declarative manifest
# (stack-manifest.env): compose profiles, service names, env vars, GPU
# requirements, bind-mount dirs. This section is fully generic - adding a new
# sidecar to the wizard requires NO launcher changes. Values are quoted
# KEY="VALUE" lines; reserved keys below, anything else is exported verbatim
# for docker-compose interpolation (e.g. LLAMA_MODEL).
$manifestPath = "voice-pipes/deploy/docker/config/stack-manifest.env"
$reservedKeys = @('COMPOSE_PROFILES', 'SIDECAR_SERVICES', 'REQUIRES_GPU', 'ENSURE_DIRS')
$manifest = @{}
if (Test-Path $manifestPath) {
    foreach ($line in Get-Content $manifestPath) {
        if ($line -match '^\s*(#|$)') { continue }
        $key, $value = $line -split '=', 2
        $manifest[$key.Trim()] = $value.Trim().Trim('"')
    }
} else {
    Write-Host "No stack manifest found ($manifestPath); starting core services only." -ForegroundColor Yellow
    Write-Host "   Re-run the wizard (or this script from the top) to configure sidecars." -ForegroundColor DarkGray
}

$profiles   = @(($manifest['COMPOSE_PROFILES'] -split ',') | Where-Object { $_ })
$sidecars   = @(($manifest['SIDECAR_SERVICES'] -split ' ') | Where-Object { $_ })
$needsGpu   = @(($manifest['REQUIRES_GPU'] -split ' ')     | Where-Object { $_ })
$ensureDirs = @(($manifest['ENSURE_DIRS'] -split ' ')      | Where-Object { $_ })

# Warn when a configured sidecar needs GPU pass-through we didn't detect.
if ($needsGpu.Count -gt 0 -and -not $gpu_supported) {
    Write-Host "Warning: $($needsGpu -join ', ') require(s) GPU pass-through, which wasn't detected." -ForegroundColor Yellow
    Write-Host "   The service(s) will start but may be unusably slow on CPU. Enable the" -ForegroundColor Yellow
    Write-Host "   NVIDIA Container Toolkit / WSL GPU support for usable latency." -ForegroundColor Yellow
}

# Pre-create bind-mount source dirs so Docker doesn't create them as
# root-owned paths.
foreach ($dir in $ensureDirs) {
    $hostDir = if ($dir -like "vendor/*") { $dir } else { Join-Path "voice-pipes" $dir }
    if (-not (Test-Path $hostDir)) { New-Item -ItemType Directory -Force -Path $hostDir | Out-Null }
}

# Export the manifest's env vars for docker-compose interpolation.
foreach ($key in $manifest.Keys) {
    if ($reservedKeys -contains $key) { continue }
    Set-Item -Path "env:$key" -Value $manifest[$key]
}

# Load API keys the wizard stored in voice-pipes/.env (the wizard runs inside
# the builder container with /app = voice-pipes/, so the file lands next to
# the crate - not in the compose project dir, where compose would auto-read
# it). The compose file passes these through by name (OPENAI_API_KEY,
# GEMINI_API_KEY, ELEVENLABS_API_KEY for the ElevenLabs Agents S2S worker,
# ASSEMBLYAI_API_KEY, ...), so they must exist in this process environment.
# Values already set in the shell win, so a deliberate override survives.
$dotEnvPath = "voice-pipes/.env"
# The shipped deploy configs set `[auth] required = true`, so the
# orchestrator and workers refuse to start without these three tokens.
# Generate each missing one once and keep it in voice-pipes/.env; the loop
# below exports them and compose passes them through by name.
$existingDotEnv = if (Test-Path $dotEnvPath) { Get-Content $dotEnvPath } else { @() }
foreach ($tok in @('VP_CLIENT_TOKEN', 'VP_ADMIN_TOKEN', 'VP_INTERNAL_TOKEN')) {
    $present = $existingDotEnv | Where-Object { $_ -match "^$tok=.+" }
    if (-not $present -and -not (Test-Path "env:$tok")) {
        $bytes = New-Object byte[] 32
        [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
        $newToken = ($bytes | ForEach-Object { $_.ToString('x2') }) -join ''
        Add-Content -Path $dotEnvPath -Value "$tok=$newToken"
        Write-Host "Generated $tok into $dotEnvPath" -ForegroundColor DarkGray
    }
}
if (Test-Path $dotEnvPath) {
    foreach ($line in Get-Content $dotEnvPath) {
        if ($line -match '^\s*(#|$)') { continue }
        $key, $value = $line -split '=', 2
        $key = $key.Trim()
        if ($key -and -not (Test-Path "env:$key")) {
            Set-Item -Path "env:$key" -Value $value.Trim().Trim('"')
        }
    }
    Write-Host "Loaded stored API keys from $dotEnvPath (shell-set values take precedence)." -ForegroundColor DarkGray
}

# Launcher policy (host-side knowledge, not the wizard's): GPU offload level
# for the llama-server sidecar. Override by setting LLAMA_GPU_LAYERS yourself.
if ($sidecars -contains 'llama-server' -and -not $env:LLAMA_GPU_LAYERS) {
    $env:LLAMA_GPU_LAYERS = if ($gpu_supported) { "99" } else { "0" }
    Write-Host "llama-server GPU layers: $env:LLAMA_GPU_LAYERS (override with LLAMA_GPU_LAYERS)" -ForegroundColor DarkGray
}

# Decide which worker to start; sidecars must be named explicitly in `up`
# (an active profile alone does not pull them into an `up` with named services).
if ($gpu_supported) {
    $worker = "worker-custom-gpu"
    $profiles += "gpu"
} else {
    $worker = "worker-custom"
}
$env:COMPOSE_PROFILES = ($profiles -join ',')

if ($sidecars.Count -gt 0) {
    Write-Host "Sidecars from manifest: $($sidecars -join ', ')" -ForegroundColor Cyan
}
Write-Host "Starting: orchestrator $worker $sidecars" -ForegroundColor Gray

# Port preflight: auto-stops stale containers from this stack (including the
# OTHER worker variant when switching CPU<->GPU) and names foreign holders.
$fleetServices = @('orchestrator', $worker) + $sidecars
if (-not (Resolve-PortConflicts -ComposeFile "voice-pipes/deploy/docker/docker-compose.yml" -Services $fleetServices -TakeForeign:$TakePorts)) {
    Write-Error "Error: Port conflicts unresolved - see messages above (or re-run with -TakePorts)."
    Exit 1
}

docker compose -f voice-pipes/deploy/docker/docker-compose.yml up --build -d --remove-orphans orchestrator $worker @sidecars

if ($LASTEXITCODE -ne 0) {
    Write-Error "Error: Failed to start backend fleet containers."
    Exit 1
}

# 6. Start the Admin UI
# The admin backend refuses to start without ADMIN_UI_TOKEN. Compose
# reads voice-pipes-admin/.env (its project dir) for interpolation, so keep
# the token there; generate one on first run and reuse it afterwards.
$adminEnvPath = "voice-pipes-admin/.env"
$adminTokenLine = if (Test-Path $adminEnvPath) { Get-Content $adminEnvPath | Where-Object { $_ -match '^ADMIN_UI_TOKEN=.+' } | Select-Object -Last 1 } else { $null }
if (-not $adminTokenLine) {
    $bytes = New-Object byte[] 32
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
    $newToken = ($bytes | ForEach-Object { $_.ToString('x2') }) -join ''
    Add-Content -Path $adminEnvPath -Value "ADMIN_UI_TOKEN=$newToken"
    $adminTokenLine = "ADMIN_UI_TOKEN=$newToken"
    Write-Host "Generated ADMIN_UI_TOKEN into $adminEnvPath (the console asks for it on first load)." -ForegroundColor DarkGray
}
$env:ADMIN_UI_TOKEN = ($adminTokenLine -split '=', 2)[1].Trim()
Write-Host "Starting Admin UI..." -ForegroundColor Gray
if (-not (Resolve-PortConflicts -ComposeFile "voice-pipes-admin/docker-compose.yml" -Services @('backend', 'frontend') -TakeForeign:$TakePorts)) {
    Write-Error "Error: Port conflicts unresolved - see messages above (or re-run with -TakePorts)."
    Exit 1
}
docker compose -f voice-pipes-admin/docker-compose.yml up --build -d
if ($LASTEXITCODE -ne 0) {
    Write-Error "Error: Failed to start Admin UI containers."
    Exit 1
}

# 6.5 Verify Worker & Fleet Readiness
Write-Host "Verifying backend fleet readiness..." -ForegroundColor Gray
$workerId = (docker compose -f voice-pipes/deploy/docker/docker-compose.yml ps -q $worker 2>$null)
if ($workerId) {
    $probeSeconds = 15
    for ($i = 0; $i -lt $probeSeconds; $i++) {
        Start-Sleep -Seconds 1
        $status = (docker inspect --format '{{.State.Status}}' $workerId 2>$null)
        if ($status -eq "restarting" -or $status -eq "exited") {
            $errLines = (docker logs --tail 25 $workerId 2>&1) | Where-Object { $_ -match '(ERROR voice_pipes|config validation failed|model load failed|missing model file)' }
            Write-Host ""
            Write-Host "[-] Worker failed startup validation (status: $status)!" -ForegroundColor Red
            if ($errLines) {
                Write-Host "   $($errLines -join "`n   ")" -ForegroundColor Yellow
            }
            Write-Host ""
            Write-Host "[i] To view full diagnostic logs: docker compose -f voice-pipes/deploy/docker/docker-compose.yml logs $worker" -ForegroundColor DarkGray
            Exit 1
        }
        if ($status -eq "running" -and $i -ge 2) {
            break
        }
    }
}

# Wait for sidecar healthchecks (e.g. pocket-tts, llama-server) before opening fleet
foreach ($sidecar in $sidecars) {
    $sidecarId = (docker compose -f voice-pipes/deploy/docker/docker-compose.yml ps -q $sidecar 2>$null)
    if ($sidecarId) {
        $hasHealth = (docker inspect --format '{{if .State.Health}}true{{else}}false{{end}}' $sidecarId 2>$null)
        if ($hasHealth -eq "true") {
            Write-Host "Waiting for sidecar '$sidecar' readiness..." -ForegroundColor Gray
            $sidecarTimeout = 180
            $ready = $false
            for ($s = 0; $s -lt $sidecarTimeout; $s += 2) {
                $hStatus = (docker inspect --format '{{.State.Health.Status}}' $sidecarId 2>$null)
                $cStatus = (docker inspect --format '{{.State.Status}}' $sidecarId 2>$null)
                if ($cStatus -eq "exited" -or $cStatus -eq "restarting") {
                    Write-Host "[-] Sidecar '$sidecar' failed to start (status: $cStatus)!" -ForegroundColor Red
                    docker compose -f voice-pipes/deploy/docker/docker-compose.yml logs --tail 30 $sidecar
                    Exit 1
                }
                if ($hStatus -eq "healthy") {
                    $ready = $true
                    Write-Host "[+] Sidecar '$sidecar' is healthy and ready." -ForegroundColor Green
                    if ($sidecar -eq "llama-server") {
                        try {
                            $warmupBody = '{"model":"default","messages":[{"role":"user","content":"hi"}],"max_tokens":1}'
                            Invoke-RestMethod -Uri "http://127.0.0.1:8081/v1/chat/completions" -Method Post -Body $warmupBody -ContentType "application/json" -TimeoutSec 10 2>$null | Out-Null
                            Write-Host "   llama-server CUDA graphs & KV cache warmed." -ForegroundColor DarkGray
                        } catch {}
                    }
                    break
                }
                Start-Sleep -Seconds 2
            }
            if (-not $ready) {
                Write-Host "[!] Warning: Sidecar '$sidecar' did not report healthy within $sidecarTimeout seconds." -ForegroundColor Yellow
            }
        }
    }
}

Write-Host ""
Write-Host "Voice Pipes Fleet started successfully!" -ForegroundColor Green
Write-Host "  Admin UI available at:  http://localhost:3000" -ForegroundColor Cyan
Write-Host "  Sign-in token: ADMIN_UI_TOKEN in $($adminEnvPath) (not printed; it would stay in this terminal's scrollback)" -ForegroundColor Cyan
Write-Host "  View token:    Get-Content $adminEnvPath" -ForegroundColor DarkGray
Write-Host "  Orchestrator API at:    http://localhost:9090" -ForegroundColor Cyan
Write-Host "  Worker WebSocket at:    ws://localhost:8800/ws" -ForegroundColor Cyan
Write-Host ""

# 7. Follow logs
Write-Host "Streaming logs from Custom Worker and Orchestrator (Press Ctrl+C to stop)..." -ForegroundColor Gray
docker compose -f voice-pipes/deploy/docker/docker-compose.yml logs -f
