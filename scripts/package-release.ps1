#requires -Version 7.2
[CmdletBinding()]
param(
    [string]$RepositoryRoot = (Split-Path -Parent $PSScriptRoot),
    [string]$ExecutablePath,
    [string]$OutputDirectory
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$RepositoryRoot = [IO.Path]::GetFullPath($RepositoryRoot)
if (-not $ExecutablePath) { $ExecutablePath = Join-Path $RepositoryRoot 'target\release\codex-hosts.exe' }
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $RepositoryRoot 'dist' }
$ExecutablePath = [IO.Path]::GetFullPath($ExecutablePath)
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$skill = Join-Path $RepositoryRoot 'skill\codex-hosts'
$archivePath = Join-Path $OutputDirectory 'codex-hosts-windows-x86_64.zip'
if ((Test-Path -LiteralPath $archivePath) -or (Test-Path -LiteralPath "$archivePath.sha256")) {
    throw "Output already exists: $archivePath. Choose a fresh output directory."
}
$required = @('SKILL.md', 'agents/openai.yaml', 'references/approvals.md', 'references/sudo.md',
    'references/su.md', 'references/hardware-keys.md', 'references/host-editor.md',
    'references/long-running.md', 'references/temporary-secrets.md', 'references/tool-protocol.md')
foreach ($relative in $required) {
    if (-not (Test-Path -LiteralPath (Join-Path $skill $relative) -PathType Leaf)) { throw "Required Skill file missing: $relative" }
}
# Include all Skill documents/metadata, including newly added references, but
# never follow a local bin link into build products or include local backups.
$documents = @((Get-Item -LiteralPath (Join-Path $skill 'SKILL.md')))
foreach ($directory in @('agents', 'references')) {
    $root = Get-Item -LiteralPath (Join-Path $skill $directory)
    $entries = @($root) + @(Get-ChildItem -LiteralPath $root.FullName -Recurse -Force)
    foreach ($item in $entries) {
        if ($item.LinkType) { throw "Skill documents must be regular files/directories, not links: $($item.FullName)" }
        if (-not $item.PSIsContainer) {
            if ($item.Extension -notin @('.md', '.yaml', '.yml')) { throw "Unexpected Skill document type: $($item.FullName)" }
            $documents += $item
        }
    }
}
$files = [ordered]@{
    'bin/codex-hosts.exe' = $ExecutablePath
    'install-local-skill.ps1' = (Join-Path $RepositoryRoot 'scripts\install-local-skill.ps1')
    'install.cmd' = (Join-Path $RepositoryRoot 'scripts\install.cmd')
}
foreach ($file in $documents) {
    if ($file.LinkType -or $file.Length -eq 0) { throw "Invalid Skill document: $($file.FullName)" }
    $relative = [IO.Path]::GetRelativePath($skill, $file.FullName).Replace('\', '/')
    $files["skill/codex-hosts/$relative"] = $file.FullName
}
foreach ($path in $files.Values) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf) -or (Get-Item -LiteralPath $path).Length -eq 0) { throw "Missing or empty package input: $path" }
}
$stream = [IO.File]::OpenRead($ExecutablePath)
try {
    if ($stream.ReadByte() -ne 0x4d -or $stream.ReadByte() -ne 0x5a) { throw 'Package executable is not a Windows PE file.' }
} finally { $stream.Dispose() }
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$stage = Join-Path $OutputDirectory ('.staging-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $stage | Out-Null
try {
    foreach ($relative in $files.Keys) {
        $destination = Join-Path $stage $relative
        New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null
        Copy-Item -LiteralPath $files[$relative] -Destination $destination
        if ((Get-FileHash -LiteralPath $destination).Hash -ne (Get-FileHash -LiteralPath $files[$relative]).Hash) { throw "Copy verification failed: $relative" }
    }
    [IO.Compression.ZipFile]::CreateFromDirectory($stage, $archivePath, [IO.Compression.CompressionLevel]::Optimal, $false)
    $zip = [IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $actual = @($zip.Entries | Where-Object Name | ForEach-Object { $_.FullName.Replace('\', '/') } | Sort-Object)
        $expected = @($files.Keys | Sort-Object)
        if (@(Compare-Object $expected $actual).Count) { throw 'Unexpected ZIP contents.' }
        foreach ($entry in $zip.Entries | Where-Object Name) {
            $inputStream = $entry.Open()
            $sha256 = [Security.Cryptography.SHA256]::Create()
            try { $hash = [Convert]::ToHexString($sha256.ComputeHash($inputStream)) }
            finally { $sha256.Dispose(); $inputStream.Dispose() }
            if ($hash -ne (Get-FileHash -LiteralPath $files[$entry.FullName.Replace('\', '/')]).Hash) { throw "ZIP content verification failed: $($entry.FullName)" }
        }
    } finally { $zip.Dispose() }
    $zipHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText("$archivePath.sha256", "$zipHash  $([IO.Path]::GetFileName($archivePath))`n")
    Write-Output "ZIP: $archivePath"
    Write-Output "Verified files ($($files.Count)): $($files.Keys -join ', ')"
    Write-Output "Executable SHA-256: $((Get-FileHash -LiteralPath $ExecutablePath).Hash)"
    Write-Output "ZIP SHA-256: $zipHash"
} catch {
    if (Test-Path -LiteralPath $archivePath) { Remove-Item -LiteralPath $archivePath }
    if (Test-Path -LiteralPath "$archivePath.sha256") { Remove-Item -LiteralPath "$archivePath.sha256" }
    throw
} finally { Remove-Item -LiteralPath $stage -Recurse -Force }
