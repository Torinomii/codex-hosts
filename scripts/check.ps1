# Runs the checks CI runs, in the same order: cheapest first, stopping at the
# first failure. CI calls each step by name, so both run the same commands.
#
#   .\scripts\check.ps1                # format, lint, test, dependency guard
#   .\scripts\check.ps1 -Step test     # one step
#   .\scripts\check.ps1 -Fix           # apply cargo fmt instead of checking it

param(
    [ValidateSet('all', 'format', 'lint', 'test', 'guard')]
    [string] $Step = 'all',
    [switch] $Fix
)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

$steps = [ordered]@{
    format = { if ($Fix) { cargo fmt --all } else { cargo fmt --all --check } }
    lint   = { cargo clippy --locked --all-targets -- -D warnings }
    # --no-fail-fast reports every failing test target, not only the first.
    test   = { cargo test --locked --all-targets --no-fail-fast }
    guard  = { & "$PSScriptRoot\check-dependency-direction.ps1" }
}

$selected = if ($Step -eq 'all') { @($steps.Keys) } else { @($Step) }
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
