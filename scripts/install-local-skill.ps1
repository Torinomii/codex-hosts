#requires -Version 7.2
<#
.SYNOPSIS
Step-by-step source build / release-package symbolic-link installer (Windows, PowerShell 7).
.DESCRIPTION
No settings are changed and no application is stopped. Existing files/directories are
renamed to backups only after confirmation. NonInteractive never implies replacement.
#>
[CmdletBinding()]
param(
    [string]$RepositoryRoot,
    [string]$CodexHome,
    [switch]$ReleasePackage,
    [switch]$Build,
    [switch]$SkipBuild,
    [switch]$Install,
    [switch]$SkipInstall,
    [switch]$ReplaceExisting,
    [switch]$NonInteractive
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-NormalizedPath([string]$Path, [string]$BasePath = (Get-Location).Path) {
    return [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetFullPath($Path, $BasePath))
}

function Get-ExistingItem([string]$Path) {
    try { return Get-Item -LiteralPath $Path -Force -ErrorAction Stop }
    catch [System.Management.Automation.ItemNotFoundException] { return $null }
}

function Get-LinkTargetPath($Item) {
    if ($null -eq $Item -or $Item.LinkType -ne 'SymbolicLink') { return $null }
    return Get-NormalizedPath ([string]$Item.Target) (Split-Path -Parent $Item.FullName)
}

function Test-ExpectedLink([string]$Path, [string]$Target) {
    return (Get-LinkTargetPath (Get-ExistingItem $Path)) -eq $Target
}

# Resolve ancestor links too, but callers choose whether to resolve the final leaf.
function Get-PhysicalPath([string]$Path) {
    $item = Get-ExistingItem $Path
    if ($null -ne $item -and ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        $resolved = $item.ResolveLinkTarget($true)
        if ($null -eq $resolved) { throw "Cannot resolve linked path: $Path" }
        return Get-NormalizedPath $resolved.FullName
    }
    $parent = Split-Path -Parent $Path
    if (-not $parent -or $parent -eq $Path) { return $Path }
    return Join-Path (Get-PhysicalPath $parent) (Split-Path -Leaf $Path)
}

function Test-Within([string]$Path, [string]$Parent) {
    return $Path -eq $Parent -or $Path.StartsWith($Parent + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)
}

function Confirm-Step([string]$Question, [bool]$Approved) {
    if ($Approved) { Write-Host "$Question [approved by parameter]"; return $true }
    if ($NonInteractive) { Write-Host "$Question [not approved; skipped]"; return $false }
    $answer = Read-Host "$Question [y/N]"
    return $null -ne $answer -and $answer.Trim() -match '^(y|yes)$'
}

function Test-BuildEnvironment([string]$Root) {
    $ready = $true
    $rustc = Get-Command rustc -CommandType Application -ErrorAction SilentlyContinue
    $cargo = Get-Command cargo -CommandType Application -ErrorAction SilentlyContinue
    $manifest = Get-Content -LiteralPath (Join-Path $Root 'Cargo.toml') -Raw
    $minimum = [regex]::Match($manifest, '(?m)^rust-version\s*=\s*"([\d.]+)"')
    if (-not $minimum.Success) { throw 'Cannot determine rust-version from Cargo.toml.' }
    if ($null -eq $rustc -or $null -eq $cargo) {
        Write-Host 'Missing Rust/Cargo. Install Rust from https://rustup.rs/ (x86_64-pc-windows-msvc), then open a new terminal.'
        $ready = $false
    } else {
        $version = & $rustc.Source --version --verbose 2>&1
        $rustExit = $LASTEXITCODE
        $cargoVersion = & $cargo.Source --version 2>&1
        $cargoExit = $LASTEXITCODE
        Write-Host ($version -join "`n")
        Write-Host ($cargoVersion -join "`n")
        $release = [regex]::Match(($version -join "`n"), '(?m)^release: (\d+\.\d+\.\d+)')
        if ($rustExit -ne 0 -or $cargoExit -ne 0 -or -not $release.Success -or
            [version]$release.Groups[1].Value -lt [version]$minimum.Groups[1].Value -or
            ($version -join "`n") -notmatch 'host: x86_64-pc-windows-msvc') {
            Write-Host "Use Rust $($minimum.Groups[1].Value)+ with the x86_64-pc-windows-msvc toolchain (rustup update stable; rustup default stable-x86_64-pc-windows-msvc)."
            $ready = $false
        }
    }
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $linker = $null
    if (Test-Path -LiteralPath $vswhere -PathType Leaf) {
        $vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        $vsExit = $LASTEXITCODE
        if ($vsExit -eq 0 -and $vs) {
            $linker = Get-ChildItem -Path (Join-Path $vs 'VC\Tools\MSVC\*\bin\Hostx64\x64\link.exe') -ErrorAction SilentlyContinue | Select-Object -Last 1
        }
    }
    $sdkRoot = Get-ItemPropertyValue -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots' -Name KitsRoot10 -ErrorAction SilentlyContinue
    $rc = $null
    if ($sdkRoot) {
        $rc = Get-ChildItem -Path (Join-Path $sdkRoot 'bin\*\x64\rc.exe') -ErrorAction SilentlyContinue | Select-Object -Last 1
    }
    if ($null -eq $linker -or $null -eq $rc) {
        Write-Host 'Missing MSVC linker or Windows SDK resource compiler. In Visual Studio Build Tools 2022+, select Desktop development with C++, MSVC x64/x86 tools and Windows 10/11 SDK.'
        Write-Host 'Download: https://visualstudio.microsoft.com/visual-cpp-build-tools/'
        $ready = $false
    } else {
        Write-Host "MSVC: $($linker.FullName)"
        Write-Host "Windows SDK: $($rc.FullName)"
    }
    return $ready
}

function Assert-Skill([string]$Source) {
    $required = @('SKILL.md', 'agents\openai.yaml', 'references\approvals.md', 'references\sudo.md',
        'references\su.md', 'references\hardware-keys.md', 'references\host-editor.md',
        'references\long-running.md', 'references\temporary-secrets.md', 'references\tool-protocol.md')
    foreach ($relative in $required) {
        $file = Join-Path $Source $relative
        if (-not (Test-Path -LiteralPath $file -PathType Leaf) -or (Get-Item -LiteralPath $file).Length -eq 0) {
            throw "Missing or empty Skill file: $file. Restore the complete source/release package."
        }
    }
}

function Assert-Executable([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "Executable missing: $Path. Build first or extract the complete release ZIP." }
    $stream = [IO.File]::OpenRead($Path)
    try {
        if ($stream.Length -lt 2 -or $stream.ReadByte() -ne 0x4d -or $stream.ReadByte() -ne 0x5a) {
            throw "Not a Windows executable: $Path"
        }
    } finally { $stream.Dispose() }
}

function Get-ItemStamp([string]$Path) {
    $item = Get-ExistingItem $Path
    if ($null -eq $item) { return '<absent>' }
    return "$($item.PSIsContainer)|$($item.LinkType)|$($item.Target)|$($item.CreationTimeUtc.Ticks)|$($item.LastWriteTimeUtc.Ticks)"
}

function New-Change([string]$Path, [string]$Target, [string]$Kind = 'Link') {
    if ($Kind -eq 'Link' -and (Test-ExpectedLink $Path $Target)) {
        Write-Host "Already current: $Path -> $Target"
        return
    }
    $item = Get-ExistingItem $Path
    $old = if ($null -eq $item) { '(not installed)' } elseif ($item.LinkType) {
        "$($item.LinkType) -> $($item.Target)"
    } elseif ($item.PSIsContainer) { 'copied directory (all contents will be kept in a backup)' } else { 'copied file (will be kept in a backup)' }
    Write-Host "`nPath: $Path"
    Write-Host "Old:  $old"
    Write-Host "New:  $(if ($Kind -eq 'BinDirectory') { 'directory containing executable symlink' } else { 'SymbolicLink' }) -> $Target"
    [pscustomobject]@{ Path = $Path; Target = $Target; Kind = $Kind; Exists = $null -ne $item
        Stamp = Get-ItemStamp $Path; Stage = ''; Backup = ''; Moved = $false; Installed = $false }
}

function Ensure-Directory([string]$Path, $Created) {
    $item = Get-ExistingItem $Path
    if ($null -ne $item) {
        if (-not $item.PSIsContainer) { throw "Not a directory: $Path" }
        return
    }
    Ensure-Directory (Split-Path -Parent $Path) $Created
    New-Item -ItemType Directory -Path $Path | Out-Null
    $Created.Add($Path)
}

function Assert-Change($Change, [string]$Path) {
    $link = if ($Change.Kind -eq 'BinDirectory') { Join-Path $Path 'codex-hosts.exe' } else { $Path }
    if (-not (Test-ExpectedLink $link $Change.Target)) { throw "Link verification failed: $link -> $($Change.Target)" }
}

# Only remove a link created by this invocation, never recursively delete an old installation.
function Remove-CreatedChange($Change, [string]$Path) {
    if ($null -eq (Get-ExistingItem $Path)) { return }
    Assert-Change $Change $Path
    if ($Change.Kind -eq 'BinDirectory') {
        if (@(Get-ChildItem -LiteralPath $Path -Force).Count -ne 1) { throw "New bin directory changed; preserving it: $Path" }
        Remove-Item -LiteralPath (Join-Path $Path 'codex-hosts.exe') -Force
        [IO.Directory]::Delete($Path)
    } else { Remove-Item -LiteralPath $Path -Force }
}

if (-not $IsWindows) { throw 'This installer requires Windows and PowerShell 7 (https://aka.ms/powershell-release?tag=stable).' }
if ($Build -and $SkipBuild) { throw 'Choose either -Build or -SkipBuild.' }
if ($Install -and $SkipInstall) { throw 'Choose either -Install or -SkipInstall.' }
if (-not [Environment]::Is64BitOperatingSystem) { throw 'The package requires 64-bit Windows (x86_64 executable).' }
if (-not $RepositoryRoot) {
    $RepositoryRoot = if (Test-Path -LiteralPath (Join-Path $PSScriptRoot 'skill\codex-hosts\SKILL.md')) { $PSScriptRoot } else { Split-Path -Parent $PSScriptRoot }
}
$RepositoryRoot = Get-PhysicalPath (Get-NormalizedPath $RepositoryRoot)
if (-not $ReleasePackage -and -not (Test-Path -LiteralPath (Join-Path $RepositoryRoot 'Cargo.toml'))) { $ReleasePackage = $true }
if ($ReleasePackage -and $Build) { throw 'Release packages already contain the program; -Build is source-only.' }
if (-not $CodexHome) {
    $CodexHome = if ($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path ([Environment]::GetFolderPath('UserProfile')) '.codex' }
}
$CodexHome = Get-NormalizedPath $CodexHome
$skillSource = Get-PhysicalPath (Join-Path $RepositoryRoot 'skill\codex-hosts')
$installedSkill = Join-Path (Get-PhysicalPath (Join-Path $CodexHome 'skills')) 'codex-hosts'
$sourceBin = Join-Path $skillSource 'bin'
$sourceExecutable = Join-Path $sourceBin 'codex-hosts.exe'
if ((Test-Within $skillSource $installedSkill) -or (Test-Within $installedSkill $skillSource)) {
    throw "Source and installation must be separate, non-nested paths: $skillSource / $installedSkill"
}

Write-Host "[1/4] Check environment and package ($RepositoryRoot)"
Write-Host "Windows / PowerShell $($PSVersionTable.PSVersion). Installation: $installedSkill"
Assert-Skill $skillSource
if ($ReleasePackage) {
    Write-Host 'Release package detected. Rust, Cargo and Visual Studio are not needed.'
    $releaseExecutable = Join-Path $RepositoryRoot 'bin\codex-hosts.exe'
    Write-Host '[2/4] Use the prebuilt release executable.'
} else {
    $ready = Test-BuildEnvironment $RepositoryRoot
    Write-Host '[2/4] Compile from source (may download Cargo dependencies; no system settings will be changed).'
    $releaseExecutable = Join-Path $RepositoryRoot 'target\x86_64-pc-windows-msvc\release\codex-hosts.exe'
    if (-not $SkipBuild -and (Confirm-Step 'Start cargo build --locked --release now?' $Build.IsPresent)) {
        if (-not $ready) { throw 'Build environment is incomplete. Follow the instructions above, reopen the terminal, and rerun; or use a release ZIP.' }
        Push-Location $RepositoryRoot
        try {
            # Explicit target and directory avoid stale output from custom Cargo environment/config.
            & cargo build --locked --release --target x86_64-pc-windows-msvc --target-dir (Join-Path $RepositoryRoot 'target')
            if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE). No installation was changed." }
        } finally { Pop-Location }
    } else {
        Write-Host 'Compilation skipped; only an existing executable can be installed.'
        if (-not (Test-Path -LiteralPath $releaseExecutable -PathType Leaf)) {
            $releaseExecutable = Join-Path $RepositoryRoot 'target\release\codex-hosts.exe'
        }
    }
}
if ($SkipInstall) { Write-Host 'Installation skipped. No links changed.'; return }
Assert-Executable $releaseExecutable
$releaseExecutable = Get-PhysicalPath $releaseExecutable
if (Test-Within $releaseExecutable $sourceBin) { throw "Executable target cannot be inside the Skill bin directory: $releaseExecutable" }
Write-Host "[3/4] Link Skill to $skillSource and its executable to $releaseExecutable"
if (-not (Confirm-Step 'Install using symbolic links?' $Install.IsPresent)) { Write-Host 'Cancelled. No installation paths changed.'; return }
$changes = [Collections.Generic.List[object]]::new()
$binItem = Get-ExistingItem $sourceBin
if ($null -ne $binItem -and (-not $binItem.PSIsContainer -or $binItem.LinkType)) {
    $change = New-Change $sourceBin $releaseExecutable 'BinDirectory'
} else {
    $change = New-Change $sourceExecutable $releaseExecutable
}
if ($null -ne $change) { $changes.Add($change) }
$change = New-Change $installedSkill $skillSource
if ($null -ne $change) { $changes.Add($change) }
foreach ($change in $changes) {
    if ($change.Exists -and -not (Confirm-Step "Update/replace the old path '$($change.Path)' with the new layout (keep backup)?" $ReplaceExisting.IsPresent)) {
        if ($NonInteractive) { throw 'Replacement requires explicit -ReplaceExisting. Nothing was changed.' }
        Write-Host 'Cancelled. No installation paths changed.'; return
    }
}

Write-Host '[4/4] Check symbolic-link capability, install, and verify (no copy fallback).'
$created = [Collections.Generic.List[string]]::new()
$operation = [Guid]::NewGuid().ToString('N')
try {
    # Stage every change before moving any existing item. This is also the actual
    # file/directory symlink capability check at each destination filesystem.
    foreach ($change in $changes) {
        $parent = Split-Path -Parent $change.Path
        Ensure-Directory $parent $created
        $leaf = Split-Path -Leaf $change.Path
        $change.Stage = Join-Path $parent ".codex-hosts.install-$operation-$leaf"
        $change.Backup = Join-Path $parent ".codex-hosts.backup-$operation-$leaf"
        if ($null -ne (Get-ExistingItem $change.Stage) -or $null -ne (Get-ExistingItem $change.Backup)) { throw 'Staging/backup path collision; rerun the installer.' }
        try {
            if ($change.Kind -eq 'BinDirectory') {
                New-Item -ItemType Directory -Path $change.Stage | Out-Null
                New-Item -ItemType SymbolicLink -Path (Join-Path $change.Stage 'codex-hosts.exe') -Target $change.Target | Out-Null
            } else {
                New-Item -ItemType SymbolicLink -Path $change.Stage -Target $change.Target | Out-Null
            }
        } catch {
            throw "Cannot create symbolic link: $($_.Exception.Message). Enable Windows Developer Mode yourself or rerun PowerShell as administrator; also check directory write permissions. No automatic setting change or copy fallback is used."
        }
        Assert-Change $change $change.Stage
    }
    foreach ($change in $changes) {
        if ((Get-ItemStamp $change.Path) -ne $change.Stamp) { throw "Path changed during installation; refusing overwrite: $($change.Path)" }
        if ($change.Exists) {
            Move-Item -LiteralPath $change.Path -Destination $change.Backup
            $change.Moved = $true
        }
        Move-Item -LiteralPath $change.Stage -Destination $change.Path
        $change.Installed = $true
        Assert-Change $change $change.Path
    }
    if (-not (Test-ExpectedLink $installedSkill $skillSource) -or -not (Test-ExpectedLink $sourceExecutable $releaseExecutable)) { throw 'Final link verification failed.' }
    Assert-Skill $installedSkill
    Assert-Executable (Join-Path $installedSkill 'bin\codex-hosts.exe')
} catch {
    $failure = $_
    $rollbackErrors = [Collections.Generic.List[string]]::new()
    for ($i = $changes.Count - 1; $i -ge 0; $i--) {
        $change = $changes[$i]
        try {
            if ($change.Installed) { Remove-CreatedChange $change $change.Path }
            if ($change.Moved) {
                if ($null -ne (Get-ExistingItem $change.Path)) { throw "Destination is occupied; backup kept at $($change.Backup)" }
                Move-Item -LiteralPath $change.Backup -Destination $change.Path
            }
        } catch { $rollbackErrors.Add($_.Exception.Message) }
    }
    if ($rollbackErrors.Count) { throw "$($failure.Exception.Message) Rollback needs attention: $($rollbackErrors -join '; ')" }
    throw $failure
} finally {
    foreach ($change in $changes) {
        if ($change.Stage -and $null -ne (Get-ExistingItem $change.Stage)) {
            # A failed symlink creation can leave only the empty staging bin dir.
            if ($change.Kind -eq 'BinDirectory' -and @(Get-ChildItem -LiteralPath $change.Stage -Force).Count -eq 0) {
                [IO.Directory]::Delete($change.Stage)
            } else { Remove-CreatedChange $change $change.Stage }
        }
    }
    for ($i = $created.Count - 1; $i -ge 0; $i--) {
        if (@(Get-ChildItem -LiteralPath $created[$i] -Force).Count -eq 0) { [IO.Directory]::Delete($created[$i]) }
    }
}
Write-Host "Installed: $installedSkill -> $skillSource"
Write-Host "Executable: $sourceExecutable -> $releaseExecutable"
foreach ($change in $changes) {
    if ($change.Moved) { Write-Host "Backup retained: $($change.Backup)" }
}
Write-Host 'Keep the source/release directory in place. If it moves, rerun this script and confirm the new paths. Restart/reload your agent when convenient to read the updated Skill.'
