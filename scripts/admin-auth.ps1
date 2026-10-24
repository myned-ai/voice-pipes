# Dot-source from any PowerShell script that calls the orchestrator admin API
# with Invoke-RestMethod / Invoke-WebRequest:
#
#     . (Join-Path $PSScriptRoot "admin-auth.ps1")            # from scripts/
#     . (Join-Path $PSScriptRoot "../../../scripts/admin-auth.ps1")
#
# The shipped deploy configs require VP_ADMIN_TOKEN on the orchestrator
# API. This resolves the token from the environment, falling back to the
# launcher-generated voice-pipes/.env, and installs it as the default
# `Authorization` header for every Invoke-RestMethod / Invoke-WebRequest call
# in the calling scope. Nothing changes for a stack that runs open (no token).

$__vpRepoRoot = Split-Path -Parent $PSScriptRoot
$__vpToken = $env:VP_ADMIN_TOKEN
if (-not $__vpToken) {
    $__vpDotEnv = Join-Path $__vpRepoRoot "voice-pipes/.env"
    if (Test-Path $__vpDotEnv) {
        $__vpLine = Get-Content $__vpDotEnv | Where-Object { $_ -match '^VP_ADMIN_TOKEN=.+' } | Select-Object -Last 1
        if ($__vpLine) { $__vpToken = ($__vpLine -split '=', 2)[1].Trim().Trim('"') }
    }
}
if ($__vpToken) {
    $__vpHeaders = @{ Authorization = "Bearer $__vpToken" }
    $PSDefaultParameterValues['Invoke-RestMethod:Headers'] = $__vpHeaders
    $PSDefaultParameterValues['Invoke-WebRequest:Headers'] = $__vpHeaders
    Write-Verbose "admin-auth: VP_ADMIN_TOKEN installed as default Authorization header"
} else {
    Write-Verbose "admin-auth: no VP_ADMIN_TOKEN (env or voice-pipes/.env); calling the orchestrator unauthenticated"
}
Remove-Variable __vpRepoRoot, __vpToken, __vpDotEnv, __vpLine, __vpHeaders -ErrorAction SilentlyContinue
