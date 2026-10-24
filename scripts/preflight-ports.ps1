# preflight-ports.ps1 - shared port-conflict resolver, dot-sourced by
# launch-stack.ps1 and rebuild-stack.ps1 before any `docker compose up`.
#
# Policy ("intelligent" beats "different ports"): the stack's host ports are
# part of its contract (browser bookmarks, ws://localhost:8800, API base), so
# we never silently remap them. Instead, for every host port the requested
# services publish:
#   1. held by a container of THIS compose project        -> stop it (a stale
#      leftover, e.g. the CPU worker when switching to the GPU variant) unless
#      it IS one of the services being started (compose recreates in place).
#   2. held by a container of a FOREIGN compose project   -> refuse with the
#      exact container/project and the command to stop it; `-TakeForeign`
#      (surfaced as -TakePorts on the launchers) stops it automatically.
#   3. held by a non-Docker host process                  -> refuse, naming
#      the process and PID.
#
# Call with the same COMPOSE_PROFILES / env the subsequent `up` will use, or
# profile-gated services will be invisible to `docker compose config`.

function Resolve-PortConflicts {
    param(
        [Parameter(Mandatory)] [string]$ComposeFile,
        [Parameter(Mandatory)] [string[]]$Services,
        [switch]$TakeForeign,
        # Report what would happen without stopping anything (for testing).
        [switch]$DryRun
    )

    # Compose's default project name is the compose file's directory name.
    $projectDir = Split-Path -Parent (Resolve-Path $ComposeFile)
    $project = (Split-Path -Leaf $projectDir).ToLower()

    $cfgRaw = docker compose -f $ComposeFile config --format json 2>$null
    $cfg = try { $cfgRaw | ConvertFrom-Json } catch { $null }
    if (-not $cfg) {
        Write-Host "  preflight: could not parse compose config for $ComposeFile; skipping port check." -ForegroundColor Yellow
        return $true
    }

    $ok = $true
    foreach ($svc in $Services) {
        $svcCfg = $cfg.services.$svc
        if (-not $svcCfg -or -not $svcCfg.ports) { continue }
        foreach ($p in $svcCfg.ports) {
            if (-not $p.published) { continue }
            $port = [int]$p.published

            # A container currently publishing this port? (Labels read via
            # `docker inspect` JSON, NOT a --format template: Windows
            # PowerShell 5.1 mangles embedded double quotes in native args,
            # which breaks Go templates like {{.Label "com.docker..."}}.)
            $name = docker ps --filter "publish=$port" --format '{{.Names}}' | Select-Object -First 1
            if ($name) {
                $meta = try { (docker inspect $name 2>$null | ConvertFrom-Json)[0] } catch { $null }
                $holderProject = "$($meta.Config.Labels.'com.docker.compose.project')"
                $holderService = "$($meta.Config.Labels.'com.docker.compose.service')"
                if ($holderProject -eq $project -and $Services -contains $holderService) {
                    continue # the service we're about to `up`; compose handles it
                }
                if ($holderProject -eq $project) {
                    Write-Host "  preflight: port $port (needed by '$svc') is held by stale container '$name' from this stack - stopping it." -ForegroundColor Yellow
                    if (-not $DryRun) { docker stop $name | Out-Null }
                } elseif ($TakeForeign) {
                    Write-Host "  preflight: port $port (needed by '$svc') is held by '$name' (compose project '$holderProject') - stopping it (-TakePorts)." -ForegroundColor Yellow
                    if (-not $DryRun) { docker stop $name | Out-Null }
                } else {
                    Write-Host "  preflight: port $port (needed by '$svc') is held by container '$name' from compose project '$holderProject'." -ForegroundColor Red
                    Write-Host "             Stop that stack (docker stop $name), or re-run with -TakePorts to do it automatically." -ForegroundColor Red
                    $ok = $false
                }
                continue
            }

            # No container publishes it - a plain host process?
            $conn = Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue |
                Select-Object -First 1
            if ($conn) {
                $procName = try { (Get-Process -Id $conn.OwningProcess -ErrorAction Stop).ProcessName } catch { "unknown" }
                if ($procName -match '^(com\.docker|docker|vpnkit|wslrelay)') {
                    # Docker holds the socket with no running container - usually a
                    # stopped container's lingering proxy; `up` normally succeeds.
                    Write-Host "  preflight: port $port is held by Docker ($procName) with no running container - continuing; if 'up' fails on this port, restart Docker Desktop." -ForegroundColor Yellow
                } else {
                    Write-Host "  preflight: port $port (needed by '$svc') is held by host process '$procName' (PID $($conn.OwningProcess))." -ForegroundColor Red
                    Write-Host "             Stop that process, or change the port mapping in $ComposeFile." -ForegroundColor Red
                    $ok = $false
                }
            }
        }
    }
    return $ok
}
