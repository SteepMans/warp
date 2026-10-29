#!/usr/bin/env pwsh
#
# Builds the OSS channel installer and publishes it to an S3-compatible bucket
# (e.g. MinIO) together with the release manifest that self-published OSS
# builds poll for updates. See app/src/autoupdate/fork_manifest.rs.
#
# Layout under s3://<Bucket>/<Prefix>/:
#   stable/manifest.json                       {"version", "url", "sha256"}
#   stable/<version>/WarpOssSetup.exe          the Inno Setup installer
#
# Required environment:
#   WARP_FORK_RELEASES_URL   Public HTTP(S) URL of s3://<Bucket>/<Prefix>, e.g.
#                            https://s3.example.com/my-bucket/warp-fork. It is
#                            baked into the binary as the update source, so it
#                            must be readable anonymously by the app.
#   WARP_FORK_S3_ENDPOINT    S3 API endpoint used for uploading, e.g.
#                            https://s3.example.com or http://127.0.0.1:9000.
#   WARP_FORK_S3_ACCESS_KEY  Access key with write access to <Bucket>/<Prefix>.
#   WARP_FORK_S3_SECRET_KEY  Secret key for the access key.
#
# Uploading uses the MinIO client `mc` (winget install MinIO.Client). The
# credentials are passed through the MC_HOST_<alias> environment variable of
# this process only; nothing is written to the mc config.
#
# Examples:
#   .\script\windows\publish_s3.ps1 -Bucket my-bucket
#   .\script\windows\publish_s3.ps1 -Bucket my-bucket -SkipBuild -Version v0.2026.10.29.12.00.oss_00
#   .\script\windows\publish_s3.ps1 -Bucket my-bucket -DryRun

Param(
    [Parameter(Mandatory = $true)]
    [String]$Bucket,

    [String]$Prefix = 'warp-fork',

    # Release tag in Warp's format: v<major>.<yyyy>.<MM>.<dd>.<HH>.<mm>.<channel>_<patch>.
    # Defaults to the current UTC time.
    [String]$Version = '',

    # Reuse an installer that was already built for -Version.
    [Switch]$SkipBuild,

    # Build and compute the manifest, but do not upload anything.
    [Switch]$DryRun
)

$ErrorActionPreference = 'Stop'

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$InstallerName = 'WarpOssSetup.exe'
$InstallerPath = Join-Path $RepoRoot "script\windows\Output\$InstallerName"

if (-not $Version) {
    $Version = 'v0.' + (Get-Date).ToUniversalTime().ToString('yyyy.MM.dd.HH.mm') + '.oss_00'
}
if ($Version -notmatch '^v\d+\.\d{4}\.\d{2}\.\d{2}\.\d{2}\.\d{2}\.[a-z]+_\d+$') {
    throw "Version '$Version' does not match v<major>.<yyyy>.<MM>.<dd>.<HH>.<mm>.<channel>_<patch>"
}

if (-not $env:WARP_FORK_RELEASES_URL) {
    throw 'WARP_FORK_RELEASES_URL is not set; it is the update source baked into the build.'
}
$ReleasesUrl = $env:WARP_FORK_RELEASES_URL.TrimEnd('/')

if (-not $SkipBuild) {
    # Inno Setup installs per user by default via winget.
    foreach ($innoDir in @("$env:LOCALAPPDATA\Programs\Inno Setup 6", "${env:ProgramFiles(x86)}\Inno Setup 6")) {
        if (Test-Path (Join-Path $innoDir 'ISCC.exe')) {
            $env:PATH = "$innoDir;$env:PATH"
            break
        }
    }
    if (-not (Get-Command ISCC -ErrorAction SilentlyContinue)) {
        throw 'ISCC.exe (Inno Setup 6) was not found. Install it with: winget install JRSoftware.InnoSetup'
    }

    Push-Location $RepoRoot
    try {
        Write-Output "Building $InstallerName $Version (update source: $ReleasesUrl)"
        & "$RepoRoot\script\windows\bundle.ps1" -CHANNEL oss -RELEASE_TAG $Version
        if (-not $?) { throw 'bundle.ps1 failed' }
    } finally {
        Pop-Location
    }
}

if (-not (Test-Path -LiteralPath $InstallerPath -PathType Leaf)) {
    throw "Installer not found at $InstallerPath"
}

$Sha256 = (Get-FileHash -LiteralPath $InstallerPath -Algorithm SHA256).Hash.ToLowerInvariant()
$InstallerUrl = "$ReleasesUrl/stable/$Version/$InstallerName"
$Manifest = [ordered]@{
    version = $Version
    url = $InstallerUrl
    sha256 = $Sha256
} | ConvertTo-Json -Compress

$ManifestPath = Join-Path ([System.IO.Path]::GetTempPath()) "warp-fork-manifest-$Version.json"
[System.IO.File]::WriteAllText($ManifestPath, $Manifest, [System.Text.UTF8Encoding]::new($false))

$InstallerSize = [math]::Round((Get-Item -LiteralPath $InstallerPath).Length / 1MB, 1)
Write-Output "Installer: $InstallerPath ($InstallerSize MB, sha256 $Sha256)"
Write-Output "Manifest:  $Manifest"

if ($DryRun) {
    Write-Output "Dry run: nothing uploaded. Manifest written to $ManifestPath"
    exit 0
}

foreach ($name in 'WARP_FORK_S3_ENDPOINT', 'WARP_FORK_S3_ACCESS_KEY', 'WARP_FORK_S3_SECRET_KEY') {
    if (-not [Environment]::GetEnvironmentVariable($name)) {
        throw "$name is not set"
    }
}
if (-not (Get-Command mc -ErrorAction SilentlyContinue)) {
    throw 'The MinIO client mc was not found. Install it with: winget install MinIO.Client'
}

$endpoint = [Uri]$env:WARP_FORK_S3_ENDPOINT
$accessKey = [Uri]::EscapeDataString($env:WARP_FORK_S3_ACCESS_KEY)
$secretKey = [Uri]::EscapeDataString($env:WARP_FORK_S3_SECRET_KEY)
$env:MC_HOST_warpfork = "$($endpoint.Scheme)://${accessKey}:${secretKey}@$($endpoint.Authority)"

try {
    $target = "warpfork/$Bucket/$($Prefix.Trim('/'))/stable"
    # Upload the installer first so the manifest never points at a missing file.
    mc cp --quiet $InstallerPath "$target/$Version/$InstallerName"
    if ($LASTEXITCODE -ne 0) { throw 'Uploading the installer failed' }
    mc cp --quiet --attr 'Cache-Control=no-cache' $ManifestPath "$target/manifest.json"
    if ($LASTEXITCODE -ne 0) { throw 'Uploading the manifest failed' }
} finally {
    Remove-Item Env:\MC_HOST_warpfork -ErrorAction SilentlyContinue
}

Write-Output "Published $Version"
Write-Output "Manifest URL: $ReleasesUrl/stable/manifest.json"
