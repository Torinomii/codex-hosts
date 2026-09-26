[CmdletBinding()]
param(
    [string]$RepositoryRoot,
    [string]$CodexHome,
    [switch]$ReleasePackage
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-NormalizedPath {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [string]$BasePath = (Get-Location).Path
    )

    $candidate = if ([System.IO.Path]::IsPathRooted($Path)) {
        $Path
    }
    else {
        [System.IO.Path]::Combine($BasePath, $Path)
    }

    $trimCharacters = [char[]]@(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    )
    return [System.IO.Path]::GetFullPath($candidate).TrimEnd($trimCharacters)
}

function Get-ExistingItem {
    param(
        [Parameter(Mandatory)]
        [string]$LiteralPath
    )

    return (Get-Item -LiteralPath $LiteralPath -Force -ErrorAction SilentlyContinue)
}

function Get-LinkTargetPath {
    param(
        [Parameter(Mandatory)]
        [System.IO.FileSystemInfo]$Item
    )

    if ($Item.LinkType -ne 'SymbolicLink') {
        return $null
    }

    $targetValue = @($Item.Target)[0]
    if ([string]::IsNullOrWhiteSpace($targetValue)) {
        return $null
    }

    $parentPath = Split-Path -Parent $Item.FullName
    return (Get-NormalizedPath -Path $targetValue -BasePath $parentPath)
}

function Test-ExpectedLink {
    param(
        [Parameter(Mandatory)]
        [string]$LinkPath,

        [Parameter(Mandatory)]
        [string]$ExpectedTarget
    )

    $item = Get-ExistingItem -LiteralPath $LinkPath
    if ($null -eq $item) {
        return $false
    }

    $actualTarget = Get-LinkTargetPath -Item $item
    if ($null -eq $actualTarget) {
        return $false
    }

    return $actualTarget.Equals(
        (Get-NormalizedPath -Path $ExpectedTarget),
        [System.StringComparison]::OrdinalIgnoreCase
    )
}

function Assert-SafeSiblingPath {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$ExpectedParent,

        [Parameter(Mandatory)]
        [string]$ExpectedNamePrefix
    )

    $fullPath = Get-NormalizedPath -Path $Path
    $parent = Get-NormalizedPath -Path (Split-Path -Parent $fullPath)
    $name = Split-Path -Leaf $fullPath

    if (-not $parent.Equals($ExpectedParent, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to modify path outside '$ExpectedParent': '$fullPath'."
    }

    if (-not $name.StartsWith($ExpectedNamePrefix, [System.StringComparison]::Ordinal)) {
        throw "Refusing to modify unexpected sibling path: '$fullPath'."
    }
}

function Remove-ExactTree {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$ExpectedParent,

        [Parameter(Mandatory)]
        [string]$ExpectedNamePrefix
    )

    Assert-SafeSiblingPath -Path $Path -ExpectedParent $ExpectedParent -ExpectedNamePrefix $ExpectedNamePrefix
    $item = Get-ExistingItem -LiteralPath $Path
    if ($null -ne $item) {
        if ($item.LinkType) {
            Remove-Item -LiteralPath $Path -Force
        }
        else {
            Remove-Item -LiteralPath $Path -Recurse -Force
        }
    }
}

function Assert-ReplaceableInstallation {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$ExpectedTarget
    )

    $installed = Get-ExistingItem -LiteralPath $Path
    if ($null -eq $installed) {
        return
    }
    if ($installed.LinkType) {
        if (-not (Test-ExpectedLink -LinkPath $Path -ExpectedTarget $ExpectedTarget)) {
            throw "Existing Skill link points elsewhere; refusing replacement: '$Path'."
        }
        return
    }
    if (-not $installed.PSIsContainer) {
        throw "Existing Skill path is not a regular directory: '$Path'."
    }

    $allowedEntries = @{
        '' = @('SKILL.md', 'agents', 'references', 'bin')
        'agents' = @('openai.yaml')
        'references' = @('hardware-keys.md', 'host-editor.md', 'long-running.md', 'temporary-secrets.md', 'tool-protocol.md')
        'bin' = @('codex-hosts.exe')
    }
    foreach ($relative in $allowedEntries.Keys) {
        $directory = if ($relative) { Join-Path $Path $relative } else { $Path }
        $item = Get-ExistingItem -LiteralPath $directory
        if ($null -eq $item) {
            continue
        }
        if (-not $item.PSIsContainer) {
            if ($relative -eq 'bin') {
                throw "Existing Skill bin path is not a directory: '$directory'."
            }
            continue
        }
        if ($relative -and $item.LinkType) {
            continue
        }
        foreach ($child in @(Get-ChildItem -LiteralPath $directory -Force)) {
            if ($child.Name -cnotin $allowedEntries[$relative]) {
                throw "Existing Skill contains an unmanaged entry; refusing replacement: '$($child.FullName)'."
            }
            $shouldBeDirectory = -not $relative -and $child.Name -in @('agents', 'references', 'bin')
            if ($child.PSIsContainer -ne $shouldBeDirectory) {
                throw "Existing Skill entry has an unexpected type: '$($child.FullName)'."
            }
        }
    }
}

if ([string]::IsNullOrWhiteSpace($RepositoryRoot)) {
    $RepositoryRoot = if ($ReleasePackage) { $PSScriptRoot } else { Split-Path -Parent $PSScriptRoot }
}
$RepositoryRoot = Get-NormalizedPath -Path $RepositoryRoot

if ([string]::IsNullOrWhiteSpace($CodexHome)) {
    if (-not [string]::IsNullOrWhiteSpace($env:CODEX_HOME)) {
        $CodexHome = $env:CODEX_HOME
    }
    else {
        $CodexHome = Join-Path ([Environment]::GetFolderPath('UserProfile')) '.codex'
    }
}
$CodexHome = Get-NormalizedPath -Path $CodexHome

$skillSource = Join-Path $RepositoryRoot 'skill\codex-hosts'
$releaseExecutable = if ($ReleasePackage) {
    Join-Path $RepositoryRoot 'bin\codex-hosts.exe'
}
else {
    Join-Path $RepositoryRoot 'target\release\codex-hosts.exe'
}
$skillFile = Join-Path $skillSource 'SKILL.md'
$agentsDirectory = Join-Path $skillSource 'agents'
$referencesDirectory = Join-Path $skillSource 'references'
$requiredSkillFiles = @(
    $skillFile,
    (Join-Path $agentsDirectory 'openai.yaml'),
    (Join-Path $referencesDirectory 'hardware-keys.md'),
    (Join-Path $referencesDirectory 'host-editor.md'),
    (Join-Path $referencesDirectory 'long-running.md'),
    (Join-Path $referencesDirectory 'temporary-secrets.md'),
    (Join-Path $referencesDirectory 'tool-protocol.md')
)

foreach ($requiredDirectory in @($RepositoryRoot, $skillSource, $agentsDirectory, $referencesDirectory)) {
    if (-not (Test-Path -LiteralPath $requiredDirectory -PathType Container)) {
        throw "Required directory does not exist: '$requiredDirectory'."
    }
}

foreach ($requiredFile in @($requiredSkillFiles + $releaseExecutable)) {
    if (-not (Test-Path -LiteralPath $requiredFile -PathType Leaf)) {
        throw "Required file does not exist: '$requiredFile'."
    }
}

if ((Get-Item -LiteralPath $releaseExecutable -Force).Length -le 0) {
    throw "Release executable is empty: '$releaseExecutable'."
}

$skillsRoot = Get-NormalizedPath -Path (Join-Path $CodexHome 'skills')
$installedSkill = Join-Path $skillsRoot 'codex-hosts'
Assert-SafeSiblingPath -Path $installedSkill -ExpectedParent $skillsRoot -ExpectedNamePrefix 'codex-hosts'
Assert-ReplaceableInstallation -Path $installedSkill -ExpectedTarget $skillSource

# Keep the executable next to the Skill through one link inside the source
# directory. The installed Skill directory itself remains a single link.
$sourceBin = Join-Path $skillSource 'bin'
$sourceExecutable = Join-Path $sourceBin 'codex-hosts.exe'
$binItem = Get-ExistingItem -LiteralPath $sourceBin
if ($null -ne $binItem) {
    if (-not $binItem.PSIsContainer -or $binItem.LinkType) {
        throw "Skill source bin path is not a regular directory: '$sourceBin'."
    }
    foreach ($child in @(Get-ChildItem -LiteralPath $sourceBin -Force)) {
        if ($child.Name -cne 'codex-hosts.exe') {
            throw "Skill source bin contains an unmanaged entry: '$($child.FullName)'."
        }
    }
}
$sourceExecutableItem = Get-ExistingItem -LiteralPath $sourceExecutable
if ($null -ne $sourceExecutableItem -and
    -not (Test-ExpectedLink -LinkPath $sourceExecutable -ExpectedTarget $releaseExecutable)) {
    throw "Skill source executable link points elsewhere; refusing replacement: '$sourceExecutable'."
}
if ($null -eq $binItem) {
    New-Item -ItemType Directory -Path $sourceBin | Out-Null
}
if ($null -eq $sourceExecutableItem) {
    New-Item -ItemType SymbolicLink -Path $sourceExecutable -Target $releaseExecutable | Out-Null
}
if (-not (Test-ExpectedLink -LinkPath $sourceExecutable -ExpectedTarget $releaseExecutable)) {
    throw "Executable link verification failed: '$sourceExecutable'."
}

if (Test-ExpectedLink -LinkPath $installedSkill -ExpectedTarget $skillSource) {
    Write-Output "Linked codex-hosts Skill is already current: $installedSkill -> $skillSource"
    Write-Output "Verified executable: $sourceExecutable -> $releaseExecutable"
    return
}

New-Item -ItemType Directory -Path $skillsRoot -Force | Out-Null

$operationId = [Guid]::NewGuid().ToString('N')
$stagingPath = Join-Path $skillsRoot ".codex-hosts.install-$operationId"
$backupPath = Join-Path $skillsRoot ".codex-hosts.backup-$operationId"
Assert-SafeSiblingPath -Path $stagingPath -ExpectedParent $skillsRoot -ExpectedNamePrefix '.codex-hosts.install-'
Assert-SafeSiblingPath -Path $backupPath -ExpectedParent $skillsRoot -ExpectedNamePrefix '.codex-hosts.backup-'

$previousMoved = $false
$newInstalled = $false

try {
    try {
        New-Item -ItemType SymbolicLink -Path $stagingPath -Target $skillSource | Out-Null
    }
    catch {
        throw "Cannot create symbolic link '$stagingPath' to '$skillSource': $($_.Exception.Message)"
    }
    if (-not (Test-ExpectedLink -LinkPath $stagingPath -ExpectedTarget $skillSource)) {
        throw "Skill directory link verification failed: '$stagingPath'."
    }

    if ($null -ne (Get-ExistingItem -LiteralPath $installedSkill)) {
        Move-Item -LiteralPath $installedSkill -Destination $backupPath
        $previousMoved = $true
    }

    Move-Item -LiteralPath $stagingPath -Destination $installedSkill
    $newInstalled = $true
    if (-not (Test-ExpectedLink -LinkPath $installedSkill -ExpectedTarget $skillSource)) {
        throw "Skill directory link verification failed: '$installedSkill'."
    }
    if (-not (Test-ExpectedLink -LinkPath (Join-Path $installedSkill 'bin\codex-hosts.exe') -ExpectedTarget $releaseExecutable)) {
        throw "Installed executable link verification failed: '$installedSkill'."
    }

    if ($previousMoved) {
        Remove-ExactTree -Path $backupPath -ExpectedParent $skillsRoot -ExpectedNamePrefix '.codex-hosts.backup-'
        $previousMoved = $false
    }
}
catch {
    $failure = $_

    if ($newInstalled -and $null -ne (Get-ExistingItem -LiteralPath $installedSkill)) {
        Remove-ExactTree -Path $installedSkill -ExpectedParent $skillsRoot -ExpectedNamePrefix 'codex-hosts'
        $newInstalled = $false
    }

    if ($previousMoved -and $null -ne (Get-ExistingItem -LiteralPath $backupPath)) {
        Move-Item -LiteralPath $backupPath -Destination $installedSkill
        $previousMoved = $false
    }

    throw $failure
}
finally {
    if ($null -ne (Get-ExistingItem -LiteralPath $stagingPath)) {
        Remove-ExactTree -Path $stagingPath -ExpectedParent $skillsRoot -ExpectedNamePrefix '.codex-hosts.install-'
    }
}

Write-Output "Installed linked codex-hosts Skill: $installedSkill -> $skillSource"
Write-Output "Verified executable: $sourceExecutable -> $releaseExecutable"
