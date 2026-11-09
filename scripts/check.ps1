# Runs offline checks with fake CLI processes, stopping at the first failure.
# The real 1Password app is used only by the explicitly selected op-live step.
# op-live only: CODEX_HOSTS_OP_LIVE (process or user environment) is
# <op.exe>|<account id>|<vault id>; the vault contains the p0-fake-host fixture.
#
#   .\scripts\check.ps1                # format, lint, test, dependency guard
#   .\scripts\check.ps1 -Step test     # one step
#   .\scripts\check.ps1 -Step op-live  # real 1Password listing and item read
#   .\scripts\check.ps1 -Fix           # apply cargo fmt instead of checking it

param(
    [ValidateSet('all', 'format', 'lint', 'test', 'op-live', 'guard')]
    [string] $Step = 'all',
    [switch] $Fix
)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

$steps = [ordered]@{
    format = { if ($Fix) { cargo fmt --all } else { cargo fmt --all --check } }
    lint   = { cargo clippy --locked --all-targets -- -D warnings }
    # --no-fail-fast reports every failing test target, not only the first.
    test   = {
        cargo build --locked --examples
        if ($LASTEXITCODE -eq 0) {
            cargo test --locked --all-targets --no-fail-fast
        }
    }
    guard  = { & "$PSScriptRoot\check-dependency-direction.ps1" }
}
$defaultSteps = @($steps.Keys)
$steps['op-live'] = {
    $previous = $env:CODEX_HOSTS_OP_LIVE
    $config = if ([string]::IsNullOrWhiteSpace($previous)) {
        [Environment]::GetEnvironmentVariable('CODEX_HOSTS_OP_LIVE', 'User')
    } else {
        $previous
    }
    if ([string]::IsNullOrWhiteSpace($config)) {
        throw 'The manual live 1Password test needs CODEX_HOSTS_OP_LIVE=<op.exe>|<account id>|<vault id>, the 1Password app and the p0-fake-host test fixture.'
    }
    try {
        $env:CODEX_HOSTS_OP_LIVE = $config
        & cargo test --locked --bin codex-hosts onepassword::tests::live_listing_and_item_read -- --ignored --exact --test-threads=1 --nocapture 2>&1 |
            Tee-Object -Variable liveOutput
        $liveExit = $LASTEXITCODE
        if ($liveExit -eq 0 -and ($liveOutput -join "`n") -notmatch '(?m)^test result: ok\. 1 passed; 0 failed; 0 ignored;') {
            throw 'The manual live 1Password test did not run and pass exactly once.'
        }
        $global:LASTEXITCODE = $liveExit
    } finally {
        if ($null -eq $previous) {
            Remove-Item Env:CODEX_HOSTS_OP_LIVE -ErrorAction SilentlyContinue
        } else {
            $env:CODEX_HOSTS_OP_LIVE = $previous
        }
    }
}

$selected = if ($Step -eq 'all') { $defaultSteps } else { @($Step) }
foreach ($name in $selected) {
    Write-Host "==> $name" -ForegroundColor Cyan
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $global:LASTEXITCODE = 0
    & $steps[$name]
    if ($LASTEXITCODE -ne 0) {
        Write-Host "==> $name failed (exit $LASTEXITCODE) after $([int]$timer.Elapsed.TotalSeconds) s" -ForegroundColor Red
        exit $LASTEXITCODE
    }
    Write-Host "==> $name passed in $([int]$timer.Elapsed.TotalSeconds) s" -ForegroundColor Green
}
