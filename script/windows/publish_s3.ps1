#!/usr/bin/env pwsh
#
# Builds the OSS channel installer and publishes it to an S3-compatible bucket
# (e.g. MinIO) together with the release manifest that self-published OSS
# builds poll for updates. See app/src/autoupdate/fork_manifest.rs.
#
# Layout under s3://<Bucket>/<Prefix>/:
#   stable/manifest.json                       {"version", "url", "sha256", "commit"?, "changelog_url"?}
#   stable/<version>/WarpOssSetup.exe          the Inno Setup installer
#   stable/<version>/changelog.json            commit titles since the previous release
#
# changelog.json lists the titles of the commits between the commit of the
# previously published manifest and HEAD, grouped by their "Module:" prefix:
#   {"version", "previous_version", "commit", "previous_commit", "date",
#    "groups": [{"module": "...", "items": ["...", ...]}, ...]}
# A shallow checkout is deepened just enough to reach the previous commit.
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
# Uploading uses the AWS CLI (winget install Amazon.AWSCLI) with path-style
# addressing. The credentials are passed through AWS_* environment variables of
# this process only; nothing is written to the AWS config.
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

function ConvertFrom-ReleaseVersion([String]$Tag) {
    # v<major>.<yyyy>.<MM>.<dd>.<HH>.<mm>.<channel>_<patch> encodes the UTC build time.
    if ($Tag -match '^v\d+\.(\d{4})\.(\d{2})\.(\d{2})\.(\d{2})\.(\d{2})\.') {
        return [DateTime]::new([int]$Matches[1], [int]$Matches[2], [int]$Matches[3], [int]$Matches[4], [int]$Matches[5], 0, [DateTimeKind]::Utc)
    }
    return $null
}

function Test-GitCommit([String]$Sha) {
    if (-not $Sha) { return $false }
    git -C $RepoRoot cat-file -e "$Sha^{commit}" 2>$null
    return $LASTEXITCODE -eq 0
}

# Fetches just enough history of a shallow checkout to reach the previous release commit:
# commits since shortly before the previous build, then a bounded deepen as a fallback.
function Expand-GitHistory([String]$PreviousCommit, $PreviousTime) {
    if ((git -C $RepoRoot rev-parse --is-shallow-repository) -ne 'true') { return }
    if (Test-GitCommit $PreviousCommit) { return }
    $head = (git -C $RepoRoot rev-parse HEAD).Trim()
    if ($PreviousTime) {
        $since = $PreviousTime.AddDays(-2).ToString('yyyy-MM-ddTHH:mm:ssZ')
        git -C $RepoRoot fetch --quiet --no-tags --filter=blob:none "--shallow-since=$since" origin $head 2>$null
        if ($LASTEXITCODE -ne 0) { Write-Warning "Fetching history since $since failed" }
    }
    if ($PreviousCommit -and -not (Test-GitCommit $PreviousCommit)) {
        git -C $RepoRoot fetch --quiet --no-tags --filter=blob:none --deepen=300 origin $head 2>$null
        if ($LASTEXITCODE -ne 0) { Write-Warning 'Deepening the checkout failed' }
    }
}

# Builds the changelog object for this release from the commit titles since the previous one.
function New-Changelog($PreviousManifest, [String]$Commit) {
    $previousVersion = $null
    $previousCommit = $null
    if ($PreviousManifest) {
        $previousVersion = [String]$PreviousManifest.version
        if ($PreviousManifest.PSObject.Properties['commit']) { $previousCommit = [String]$PreviousManifest.commit }
    }
    $previousTime = if ($previousVersion) { ConvertFrom-ReleaseVersion $previousVersion } else { $null }
    Expand-GitHistory $previousCommit $previousTime

    if (Test-GitCommit $previousCommit) {
        $range = @("$previousCommit..$Commit")
    } elseif ($previousTime) {
        # Manifests published before the commit field existed: fall back to the build time.
        $range = @("--since=$($previousTime.ToString('yyyy-MM-ddTHH:mm:ssZ'))", $Commit)
    } else {
        $range = @('--max-count=30', $Commit)
    }

    # git prints UTF-8; make PowerShell decode it as such (commit titles are Russian).
    $previousEncoding = [Console]::OutputEncoding
    [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
    try {
        $titles = @(git -C $RepoRoot log --no-merges --format=%s @range)
        if ($LASTEXITCODE -ne 0) { throw 'git log failed' }
    } finally {
        [Console]::OutputEncoding = $previousEncoding
    }

    # "форматирование" (formatting) and merge commits carry no user-visible change.
    $skipPattern = '(?i)^merge\s|' + [regex]::Unescape('форматировани')
    # "Прочее" (other) collects titles without a "Module:" prefix.
    $otherModule = [regex]::Unescape('Прочее')
    $groups = [ordered]@{}
    foreach ($title in $titles) {
        $title = $title.Trim()
        if (-not $title -or $title -match $skipPattern) { continue }
        if ($title -match '^(?<module>[^:\s][^:]{0,39}):\s+(?<item>.+)$') {
            $module = $Matches['module'].Trim()
            $item = $Matches['item'].Trim()
        } else {
            $module = $otherModule
            $item = $title
        }
        if (-not $groups.Contains($module)) {
            $groups[$module] = [System.Collections.Generic.List[String]]::new()
        }
        $groups[$module].Add($item)
    }

    $buildTime = ConvertFrom-ReleaseVersion $Version
    if (-not $buildTime) { $buildTime = (Get-Date).ToUniversalTime() }
    return [ordered]@{
        version = $Version
        previous_version = $previousVersion
        commit = $Commit
        previous_commit = $previousCommit
        date = $buildTime.ToString('yyyy-MM-ddTHH:mm:ssZ')
        groups = @($groups.Keys | ForEach-Object { [ordered]@{ module = $_; items = @($groups[$_]) } })
    }
}

$Sha256 = (Get-FileHash -LiteralPath $InstallerPath -Algorithm SHA256).Hash.ToLowerInvariant()
$InstallerUrl = "$ReleasesUrl/stable/$Version/$InstallerName"
$Commit = (git -C $RepoRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or -not $Commit) { throw 'Unable to determine the release commit' }

# The previous release, to list what changed since. Unreachable or missing manifests just
# mean there is no previous release to compare against.
$PreviousManifest = $null
try {
    $PreviousManifest = Invoke-RestMethod -Uri "$ReleasesUrl/stable/manifest.json?r=$([guid]::NewGuid().ToString('N'))" -TimeoutSec 30
    Write-Output "Previous release: $($PreviousManifest.version)"
} catch {
    Write-Warning "No previous release manifest: $($_.Exception.Message)"
}

$ChangelogPath = $null
$ChangelogUrl = $null
try {
    $Changelog = New-Changelog $PreviousManifest $Commit
    $ChangelogPath = Join-Path ([System.IO.Path]::GetTempPath()) "warp-fork-changelog-$Version.json"
    [System.IO.File]::WriteAllText($ChangelogPath, ($Changelog | ConvertTo-Json -Depth 5), [System.Text.UTF8Encoding]::new($false))
    $ChangelogUrl = "$ReleasesUrl/stable/$Version/changelog.json"
    $itemCount = ($Changelog.groups | ForEach-Object { $_.items.Count } | Measure-Object -Sum).Sum
    Write-Output "Changelog: $itemCount commits in $($Changelog.groups.Count) modules since $($Changelog.previous_version)"
} catch {
    # A release without a changelog is still a valid release.
    Write-Warning "Changelog generation failed: $($_.Exception.Message)"
    $ChangelogPath = $null
    $ChangelogUrl = $null
}

$ManifestData = [ordered]@{
    version = $Version
    url = $InstallerUrl
    sha256 = $Sha256
    commit = $Commit
}
if ($ChangelogUrl) { $ManifestData['changelog_url'] = $ChangelogUrl }
$Manifest = $ManifestData | ConvertTo-Json -Compress

$ManifestPath = Join-Path ([System.IO.Path]::GetTempPath()) "warp-fork-manifest-$Version.json"
[System.IO.File]::WriteAllText($ManifestPath, $Manifest, [System.Text.UTF8Encoding]::new($false))

$InstallerSize = [math]::Round((Get-Item -LiteralPath $InstallerPath).Length / 1MB, 1)
Write-Output "Installer: $InstallerPath ($InstallerSize MB, sha256 $Sha256)"
Write-Output "Manifest:  $Manifest"
if ($ChangelogPath) { Write-Output "Changelog written to $ChangelogPath" }

if ($DryRun) {
    Write-Output "Dry run: nothing uploaded. Manifest written to $ManifestPath"
    exit 0
}

foreach ($name in 'WARP_FORK_S3_ENDPOINT', 'WARP_FORK_S3_ACCESS_KEY', 'WARP_FORK_S3_SECRET_KEY') {
    if (-not [Environment]::GetEnvironmentVariable($name)) {
        throw "$name is not set"
    }
}
if (-not (Get-Command aws -ErrorAction SilentlyContinue)) {
    throw 'The AWS CLI was not found. Install it with: winget install Amazon.AWSCLI'
}

$env:AWS_ACCESS_KEY_ID = $env:WARP_FORK_S3_ACCESS_KEY
$env:AWS_SECRET_ACCESS_KEY = $env:WARP_FORK_S3_SECRET_KEY
$env:AWS_DEFAULT_REGION = 'us-east-1'
$env:AWS_EC2_METADATA_DISABLED = 'true'
$env:AWS_CONFIG_FILE = Join-Path ([System.IO.Path]::GetTempPath()) 'warp-fork-aws-config'
[System.IO.File]::WriteAllText($env:AWS_CONFIG_FILE, "[default]`ns3 =`n    addressing_style = path`n")

try {
    $endpoint = $env:WARP_FORK_S3_ENDPOINT.TrimEnd('/')
    $target = "s3://$Bucket/$($Prefix.Trim('/'))/stable"
    # Upload the installer first so the manifest never points at a missing file.
    aws --endpoint-url $endpoint s3 cp --only-show-errors $InstallerPath "$target/$Version/$InstallerName" --content-type application/octet-stream
    if ($LASTEXITCODE -ne 0) { throw 'Uploading the installer failed' }
    if ($ChangelogPath) {
        aws --endpoint-url $endpoint s3 cp --only-show-errors $ChangelogPath "$target/$Version/changelog.json" --content-type 'application/json; charset=utf-8'
        if ($LASTEXITCODE -ne 0) { throw 'Uploading the changelog failed' }
    }
    aws --endpoint-url $endpoint s3 cp --only-show-errors $ManifestPath "$target/manifest.json" --content-type application/json --cache-control no-cache
    if ($LASTEXITCODE -ne 0) { throw 'Uploading the manifest failed' }
} finally {
    foreach ($name in 'AWS_ACCESS_KEY_ID', 'AWS_SECRET_ACCESS_KEY', 'AWS_CONFIG_FILE') {
        Remove-Item "Env:\$name" -ErrorAction SilentlyContinue
    }
}

Write-Output "Published $Version"
Write-Output "Manifest URL: $ReleasesUrl/stable/manifest.json"
