# Fails when the core depends on something its boundary forbids.
#
# Boundary (see specs/003-mcp-server/design.md §0.5): codex-hosts talks to
# Bitwarden only by running the official native bw.exe as a child process, so
# the core tree must never pull in an HTTP/TLS stack or a vault SDK. It only
# opens SSH/Telnet connections and local pipes itself.
#
# Run from the repository root: .\scripts\check-dependency-direction.ps1

$ErrorActionPreference = 'Stop'

function Get-TreeNames([string] $Package) {
    $lines = & cargo tree -p $Package -e normal,build --prefix none --no-dedupe --locked
    if ($LASTEXITCODE -ne 0) {
        throw "cargo tree failed for package '$Package'"
    }
    $lines |
        ForEach-Object { ($_.Trim() -split '\s+')[0] } |
        Where-Object { $_ } |
        Sort-Object -Unique
}

$forbidden = @(
    'reqwest', 'hyper', 'hyper-util', 'h2', 'http', 'http-body',
    'rustls', 'rustls-pki-types', 'native-tls', 'openssl', 'openssl-sys',
    'ureq', 'isahc', 'attohttpc', 'curl', 'curl-sys', 'surf',
    'tungstenite', 'tokio-tungstenite',
    'bitwarden', 'bitwarden-core', 'bitwarden-crypto', 'bitwarden-vault', 'bitwarden-pm', 'ap-client'
)

$names = Get-TreeNames 'codex-hosts'
$hits = @($forbidden | Where-Object { $names -contains $_ })
if ($hits.Count -gt 0) {
    Write-Host "FAIL codex-hosts: depends on $($hits -join ', ') -- the core must not embed an HTTP/TLS stack or a vault SDK"
    exit 1
}
Write-Host "OK   codex-hosts: $($names.Count) packages, boundary respected"
