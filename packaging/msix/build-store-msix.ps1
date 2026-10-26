[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$PackageIdentityName,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$Publisher,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$PublisherDisplayName,

    [string]$DisplayName = 'AI Usage Monitor',

    [string]$Description = 'Monitor usage limits and budgets for supported AI services.',

    [switch]$SkipBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$targetRoot = [System.IO.Path]::GetFullPath((Join-Path $repoRoot 'target\msix-store'))
$cargoTargetRoot = Join-Path $targetRoot 'cargo-target'
$stagingRoot = Join-Path $targetRoot 'staging'
$manifestSource = Join-Path $PSScriptRoot 'AppxManifest.xml'
$assetsSource = Join-Path $PSScriptRoot 'Assets'
$cargoToml = Join-Path $repoRoot 'Cargo.toml'
$binaryPath = Join-Path $cargoTargetRoot 'release\claude-code-usage-monitor.exe'
$bridgeBinaryPath = Join-Path $cargoTargetRoot 'release\aum-quota.exe'

function Assert-PathUnderTarget([string]$Path) {
    $fullPath = [System.IO.Path]::GetFullPath($Path)
    $targetPrefix = $targetRoot.TrimEnd('\') + '\'
    if (-not $fullPath.StartsWith($targetPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to modify a path outside the Store MSIX target directory: $fullPath"
    }
}

function Find-WindowsSdkTool([string]$Name) {
    $sdkBin = 'C:\Program Files (x86)\Windows Kits\10\bin'
    $candidate = Get-ChildItem -LiteralPath $sdkBin -Directory -ErrorAction Stop |
        Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' } |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName "x64\$Name" } |
        Where-Object { Test-Path -LiteralPath $_ } |
        Select-Object -First 1

    if (-not $candidate) {
        throw "Unable to locate $Name in the Windows 10/11 SDK."
    }
    return $candidate
}

function Get-StoreMsixVersion([string]$CargoManifestPath) {
    $manifestText = Get-Content -Raw -LiteralPath $CargoManifestPath
    $match = [regex]::Match(
        $manifestText,
        '(?m)^version\s*=\s*"(?<version>[^"]+)"\s*$'
    )

    if (-not $match.Success) {
        throw 'Unable to read the Cargo package version.'
    }

    $coreMatch = [regex]::Match(
        $match.Groups['version'].Value,
        '^(?<major>\d+)\.(?<minor>\d+)\.(?<patch>\d+)(?<suffix>[-+].*)?$'
    )

    if (-not $coreMatch.Success) {
        throw "Cargo version '$($match.Groups['version'].Value)' is not supported."
    }

    $parts = @(
        [int]$coreMatch.Groups['major'].Value,
        [int]$coreMatch.Groups['minor'].Value,
        [int]$coreMatch.Groups['patch'].Value
    )

    foreach($part in $parts){
        if($part -lt 0 -or $part -gt 65535){
            throw 'MSIX version components must be between 0 and 65535.'
        }
    }

    if($parts[0] -eq 0){
        throw 'The first MSIX version component cannot be 0 for a Store package.'
    }

    return "$($parts[0]).$($parts[1]).$($parts[2]).0"
}

$msixVersion=Get-StoreMsixVersion -CargoManifestPath $cargoToml
$features=@('antigravity')

if($features -contains 'self-update'){
    throw 'The Store MSIX build must never include the self-update feature.'
}

if(-not $SkipBuild){
    $cargo=Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'

    if(-not (Test-Path -LiteralPath $cargo)){
        throw "Cargo was not found at $cargo"
    }

    & $cargo build `
        --release `
        --locked `
        --no-default-features `
        --features ($features -join ',') `
        --target-dir $cargoTargetRoot

    if($LASTEXITCODE -ne 0){
        throw "Cargo release build failed with exit code $LASTEXITCODE."
    }
}

foreach($required in @(
    $binaryPath,
    $bridgeBinaryPath,
    $manifestSource,
    $assetsSource
)){
    if(-not (Test-Path -LiteralPath $required)){
        throw "Required Store package input was not found: $required"
    }
}

Assert-PathUnderTarget $stagingRoot

if(Test-Path -LiteralPath $stagingRoot){
    Remove-Item -LiteralPath $stagingRoot -Recurse -Force
}

New-Item -ItemType Directory -Path $stagingRoot -Force | Out-Null
Copy-Item -LiteralPath $binaryPath -Destination $stagingRoot
Copy-Item -LiteralPath $bridgeBinaryPath -Destination $stagingRoot
Copy-Item -LiteralPath $assetsSource -Destination $stagingRoot -Recurse
Copy-Item -LiteralPath $manifestSource -Destination (Join-Path $stagingRoot 'AppxManifest.xml')

$stagedManifestPath=Join-Path $stagingRoot 'AppxManifest.xml'
[xml]$manifest=Get-Content -Raw -LiteralPath $stagedManifestPath

$ns=New-Object System.Xml.XmlNamespaceManager($manifest.NameTable)
$ns.AddNamespace('f','http://schemas.microsoft.com/appx/manifest/foundation/windows10')
$ns.AddNamespace('uap','http://schemas.microsoft.com/appx/manifest/uap/windows10')

$identity=$manifest.SelectSingleNode('/f:Package/f:Identity',$ns)
$identity.SetAttribute('Name',$PackageIdentityName)
$identity.SetAttribute('Publisher',$Publisher)
$identity.SetAttribute('Version',$msixVersion)

$manifest.SelectSingleNode('/f:Package/f:Properties/f:DisplayName',$ns).InnerText=$DisplayName
$manifest.SelectSingleNode('/f:Package/f:Properties/f:PublisherDisplayName',$ns).InnerText=$PublisherDisplayName
$manifest.SelectSingleNode('/f:Package/f:Properties/f:Description',$ns).InnerText=$Description

foreach($visual in $manifest.SelectNodes('/f:Package/f:Applications/f:Application/uap:VisualElements',$ns)){
    $visual.SetAttribute('DisplayName',$DisplayName)
    $visual.SetAttribute('Description',$Description)
}

$manifest.Save($stagedManifestPath)

$makeAppx=Find-WindowsSdkTool 'makeappx.exe'
$packagePath=Join-Path $targetRoot "AIUsageMonitor.Store_${msixVersion}_x64.msix"

Assert-PathUnderTarget $packagePath

if(Test-Path -LiteralPath $packagePath){
    Remove-Item -LiteralPath $packagePath -Force
}

& $makeAppx pack /d $stagingRoot /p $packagePath /o

if($LASTEXITCODE -ne 0){
    throw "MakeAppx failed with exit code $LASTEXITCODE."
}

[pscustomobject]@{
    Package=$packagePath
    Version=$msixVersion
    PackageIdentityName=$PackageIdentityName
    Publisher=$Publisher
    PublisherDisplayName=$PublisherDisplayName
    Features=($features -join ',')
    SelfUpdateIncluded=$false
    LocallySigned=$false
}
