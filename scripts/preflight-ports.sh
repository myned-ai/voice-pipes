#!/bin/bash
# preflight-ports.sh — shared port-conflict resolver, sourced by
# launch-stack.sh before any `docker compose up`. Bash port of
# scripts/preflight-ports.ps1 — keep the two in sync.
#
# Policy ("intelligent" beats "different ports"): the stack's host ports are
# part of its contract (browser bookmarks, ws://localhost:8800, API base), so
# we never silently remap them. For every host port the requested services
# publish:
#   1. held by a container of THIS compose project        -> stop it (a stale
#      leftover, e.g. the CPU worker when switching to the GPU variant) unless
#      it IS one of the services being started (compose recreates in place).
#   2. held by a container of a FOREIGN compose project   -> refuse with the
#      exact container/project; take_foreign=1 (--take-ports) stops it.
#   3. held by a non-Docker host process                  -> refuse, naming it
#      as far as the available tools (ss/lsof/netstat) allow.
#
# Call with the same COMPOSE_PROFILES / env the subsequent `up` will use, or
# profile-gated services are invisible to `docker compose config`.
# Set PREFLIGHT_DRY_RUN=1 to report without stopping anything (for testing).

# usage: resolve_port_conflicts <compose-file> <take_foreign 0|1> <service>...
resolve_port_conflicts() {
    local compose_file="$1" take_foreign="$2"
    shift 2
    local services=("$@")

    # Compose's default project name is the compose file's directory name.
    local project
    project=$(basename "$(cd "$(dirname "$compose_file")" && pwd)" | tr '[:upper:]' '[:lower:]')

    local cfg
    cfg=$(docker compose -f "$compose_file" config --format json 2>/dev/null)
    if [ -z "$cfg" ]; then
        echo "  preflight: could not read compose config for $compose_file; skipping port check."
        return 0
    fi

    # Extract "service port" pairs for the requested services (jq preferred,
    # python fallback; without either, skip with a notice rather than guess).
    local pairs
    if command -v jq >/dev/null 2>&1; then
        pairs=$(printf '%s' "$cfg" | jq -r '.services | to_entries[] | .key as $s | (.value.ports // [])[] | select(.published) | "\($s) \(.published)"')
    elif command -v python3 >/dev/null 2>&1 || command -v python >/dev/null 2>&1; then
        local py; py=$(command -v python3 || command -v python)
        pairs=$(printf '%s' "$cfg" | "$py" -c '
import json, sys
d = json.load(sys.stdin)
for name, svc in d.get("services", {}).items():
    for p in (svc.get("ports") or []):
        pub = p.get("published")
        if pub:
            print(name, pub)
')
    else
        echo "  preflight: neither jq nor python available; skipping port check."
        return 0
    fi

    local ok=0 svc port line
    while IFS=' ' read -r svc port; do
        [ -z "$port" ] && continue
        case " ${services[*]} " in *" $svc "*) ;; *) continue ;; esac

        # A container currently publishing this port?
        local holder
        holder=$(docker ps --filter "publish=$port" \
            --format '{{.Names}}|{{.Label "com.docker.compose.project"}}|{{.Label "com.docker.compose.service"}}' | head -n1)
        if [ -n "$holder" ]; then
            local name hproj hsvc
            IFS='|' read -r name hproj hsvc <<<"$holder"
            if [ "$hproj" = "$project" ]; then
                case " ${services[*]} " in *" $hsvc "*) continue ;; esac # our own target service
                echo "  preflight: port $port (needed by '$svc') is held by stale container '$name' from this stack — stopping it."
                [ "${PREFLIGHT_DRY_RUN:-0}" = 1 ] || docker stop "$name" >/dev/null
            elif [ "$take_foreign" = 1 ]; then
                echo "  preflight: port $port (needed by '$svc') is held by '$name' (compose project '$hproj') — stopping it (--take-ports)."
                [ "${PREFLIGHT_DRY_RUN:-0}" = 1 ] || docker stop "$name" >/dev/null
            else
                echo "  preflight: port $port (needed by '$svc') is held by container '$name' from compose project '$hproj'." >&2
                echo "             Stop that stack (docker stop $name), or re-run with --take-ports to do it automatically." >&2
                ok=1
            fi
            continue
        fi

        # No container publishes it — is anything listening at all?
        if ! (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
            continue # free
        fi
        exec 3>&- 3<&- 2>/dev/null

        # Busy, but not a running container: attribute best-effort.
        local who=""
        if command -v ss >/dev/null 2>&1; then
            who=$(ss -ltnp 2>/dev/null | awk -v p=":$port" '$4 ~ p"$" {print $NF; exit}')
        elif command -v lsof >/dev/null 2>&1; then
            who=$(lsof -nP -iTCP:"$port" -sTCP:LISTEN 2>/dev/null | awk 'NR==2 {print $1" (pid "$2")"}')
        elif command -v netstat >/dev/null 2>&1; then
            who=$(netstat -ltnp 2>/dev/null | awk -v p=":$port" '$4 ~ p"$" {print $NF; exit}')
        fi
        case "$who" in
            *docker*|*com.docker*|*vpnkit*|*wslrelay*)
                # Docker holds the socket with no running container — usually a
                # stopped container's lingering proxy; 'up' normally succeeds.
                echo "  preflight: port $port is held by Docker ($who) with no running container — continuing; if 'up' fails on this port, restart Docker."
                ;;
            *)
                echo "  preflight: port $port (needed by '$svc') is held by host process ${who:-'(unknown — install ss or lsof for details)'}." >&2
                echo "             Stop that process, or change the port mapping in $compose_file." >&2
                ok=1
                ;;
        esac
    done <<<"$pairs"

    return $ok
}
