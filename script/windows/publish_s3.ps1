#!/usr/bin/env pwsh
#
# Builds the OSS channel installer and publishes it to an S3-compatible bucket
# (e.g. MinIO) together with the release manifest that self-published OSS
# builds poll for updates. See app/src/autoupdate/fork_manifest.rs.
#
# Layout under s3://<Bucket>/<Prefix>/:
#   stable/manifest.json                       {"version", "url", "sha256", "commit"?, "changelog_url"?}
#   stable/<version>/WarpOssSetup.exe          the Inno Setup installer
#   stable/<version>/changelog.json            what changed since the previous release
#   stable/<version>/images/<name>             pictures of the release notes
# Only the published version is kept: after the manifest is uploaded, the folders
# of older versions are removed, so their installer links stop working.
#
# changelog.json holds the release notes written by people: the changes/*.md
# fragments (format in changes/README.md) added or edited between the commit of
# the previously published manifest and HEAD. Their pictures are uploaded next to
# the installer; image_url is the first of them:
#   {"version", "previous_version", "commit", "previous_commit", "date",
#    "image_url", "notes": [{"section", "title", "text", "image_url"}, ...],
#    "groups": []}
# Without fragments, "groups" lists the commit titles of the same range grouped
# by their "Module:" prefix ({"module": "...", "items": ["...", ...]}), without
# documentation, CI and test commits. A shallow checkout is deepened just enough
# to reach the previous commit.
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
#   .\script\windows\publish_s3.ps1 -Bucket my-bucket -ChangelogOnly
#   .\script\windows\publish_s3.ps1 -Bucket my-bucket -ChangelogOnly -PreviousCommit HEAD~20

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
    [Switch]$DryRun,

    # Only print the changelog.json this release would get: no build, no upload.
    [Switch]$ChangelogOnly,

    # List the changes since this commit instead of the previously published release.
    [String]$PreviousCommit = ''
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

if (-not $SkipBuild -and -not $ChangelogOnly) {
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

if (-not $ChangelogOnly -and -not (Test-Path -LiteralPath $InstallerPath -PathType Leaf)) {
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

# Runs git with UTF-8 output decoded as such (commit titles and notes are Russian) and
# unquoted non-ASCII paths.
function Invoke-GitUtf8 {
    $previousEncoding = [Console]::OutputEncoding
    [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
    try {
        $output = @(git -C $RepoRoot -c core.quotepath=false @args)
        if ($LASTEXITCODE -ne 0) { throw "git $($args[0]) failed" }
        return $output
    } finally {
        [Console]::OutputEncoding = $previousEncoding
    }
}

# Content types of the pictures a release note may have.
$ImageContentTypes = @{ '.png' = 'image/png'; '.jpg' = 'image/jpeg'; '.jpeg' = 'image/jpeg'; '.gif' = 'image/gif'; '.webp' = 'image/webp' }
$NoteSections = @('new', 'improved', 'fixed')

# Reads a changes/*.md fragment: a frontmatter block between "---" lines with "key: value"
# pairs (section, image; "#" starts a comment), then a "## Title" heading and the text.
# Returns $null, with a warning, for a fragment without a title.
function Read-ChangeFragment([String]$RelativePath) {
    $path = Join-Path $RepoRoot $RelativePath
    $content = [System.IO.File]::ReadAllText($path, [System.Text.UTF8Encoding]::new($false))
    $content = $content.TrimStart([char]0xFEFF) -replace "`r`n?", "`n"

    $meta = @{}
    $body = $content
    if ($content -match '^---[ \t]*\n(?<meta>(?:.*\n)*?)---[ \t]*(?:\n|$)') {
        $metaBlock = $Matches['meta']
        $body = $content.Substring($Matches[0].Length)
        foreach ($line in $metaBlock -split "`n") {
            $line = ($line -replace '(^|\s)#.*$', '').Trim()
            if ($line -match '^(?<key>[A-Za-z_]+)\s*:\s*(?<value>.*)$') {
                $meta[$Matches['key'].ToLowerInvariant()] = $Matches['value'].Trim().Trim('"', "'")
            }
        }
    }

    $title = $null
    $text = [System.Collections.Generic.List[String]]::new()
    foreach ($line in $body -split "`n") {
        if ($null -eq $title) {
            if ($line -match '^\s*#{1,6}\s+(?<title>.+?)\s*#*\s*$') { $title = $Matches['title'] }
            elseif ($line.Trim()) { break }
        } else {
            $text.Add($line.TrimEnd())
        }
    }
    if (-not $title) {
        Write-Warning "$RelativePath has no '## Title' heading; skipped"
        return $null
    }

    $section = if ($meta.ContainsKey('section')) { $meta['section'].ToLowerInvariant() } else { '' }
    if ($section -notin $NoteSections) {
        Write-Warning "$RelativePath has section '$section', expected one of $($NoteSections -join ', '); using improved"
        $section = 'improved'
    }
    $image = if ($meta.ContainsKey('image') -and $meta['image']) { $meta['image'] -replace '\\', '/' } else { $null }
    if (-not $image) { Write-Warning "$RelativePath has no image" }
    return [ordered]@{
        path = $RelativePath
        section = $section
        title = $title.Trim()
        text = ConvertTo-NoteText $text
        image = $image
    }
}

# Joins the wrapped lines of each paragraph: the app shows every source line as a line of its
# own. Blank lines (paragraphs) and list items stay on separate lines.
function ConvertTo-NoteText([String[]]$Lines) {
    $result = [System.Text.StringBuilder]::new()
    $previousBlank = $true
    foreach ($line in $Lines) {
        $line = $line.Trim()
        if (-not $line) {
            if (-not $previousBlank) { [void]$result.Append("`n") }
            $previousBlank = $true
            continue
        }
        if ($result.Length -gt 0) {
            $isListItem = $line -match '^([-*+]|\d+[.)])\s'
            [void]$result.Append($(if ($previousBlank -or $isListItem) { "`n" } else { ' ' }))
        }
        [void]$result.Append($line)
        $previousBlank = $false
    }
    return $result.ToString().Trim()
}

# Resolves the picture of a fragment to a file inside the repository, or $null with a warning.
function Resolve-NoteImage([String]$Fragment, [String]$Image) {
    $full = [System.IO.Path]::GetFullPath((Join-Path $RepoRoot $Image))
    $root = [System.IO.Path]::GetFullPath($RepoRoot).TrimEnd('\', '/') + [System.IO.Path]::DirectorySeparatorChar
    if (-not $full.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) {
        Write-Warning "${Fragment}: image $Image is outside the repository; skipped"
        return $null
    }
    if (-not (Test-Path -LiteralPath $full -PathType Leaf)) {
        Write-Warning "${Fragment}: image $Image does not exist; skipped"
        return $null
    }
    $extension = [System.IO.Path]::GetExtension($full).ToLowerInvariant()
    if (-not $ImageContentTypes.ContainsKey($extension)) {
        Write-Warning "${Fragment}: image $Image is not PNG, JPEG, GIF or WebP; skipped"
        return $null
    }
    $sizeKb = [math]::Round((Get-Item -LiteralPath $full).Length / 1KB)
    if ($sizeKb -gt 300) { Write-Warning "${Fragment}: image $Image is $sizeKb KB, keep pictures under 300 KB" }
    return $full
}

# Release notes from the fragments added or edited in the release range. Fills
# $script:ChangelogImages with the pictures to upload (Path, Name, ContentType).
function Get-ReleaseNotes([String]$PreviousCommit, [String[]]$Range, [String]$Commit) {
    if (Test-GitCommit $PreviousCommit) {
        $paths = Invoke-GitUtf8 diff --no-renames --name-only --diff-filter=AM "$PreviousCommit..$Commit" -- 'changes/*.md'
    } else {
        $paths = Invoke-GitUtf8 log --no-renames --diff-filter=AM --name-only --format= @Range -- 'changes/*.md'
    }
    $paths = @($paths | ForEach-Object { $_.Trim() } |
        Where-Object { $_ -match '^changes/[^/]+\.md$' -and $_ -ne 'changes/README.md' -and (Test-Path -LiteralPath (Join-Path $RepoRoot $_) -PathType Leaf) } |
        Sort-Object -Unique)

    $notes = [System.Collections.Generic.List[Object]]::new()
    $names = @{}
    foreach ($path in $paths) {
        $fragment = Read-ChangeFragment $path
        if (-not $fragment) { continue }
        $imageUrl = $null
        if ($fragment.image) {
            $file = Resolve-NoteImage $path $fragment.image
            if ($file) {
                $key = $file.ToLowerInvariant()
                if (-not $names.ContainsKey($key)) {
                    $baseName = [System.IO.Path]::GetFileNameWithoutExtension($file) -replace '[^A-Za-z0-9._-]', '-'
                    $extension = [System.IO.Path]::GetExtension($file).ToLowerInvariant()
                    $name = "$baseName$extension"
                    $suffix = 2
                    while ($names.Values -contains $name) { $name = "$baseName-$suffix$extension"; $suffix++ }
                    $names[$key] = $name
                    $script:ChangelogImages.Add([ordered]@{ Path = $file; Name = $name; ContentType = $ImageContentTypes[$extension] })
                }
                $imageUrl = "$ReleasesUrl/stable/$Version/images/$($names[$key])"
            }
        }
        $notes.Add([ordered]@{ section = $fragment.section; title = $fragment.title; text = $fragment.text; image_url = $imageUrl })
    }
    # New features first, then improvements and fixes; fragments by name within a section.
    return @($NoteSections | ForEach-Object { $section = $_; $notes | Where-Object { $_.section -eq $section } })
}

# Builds the changelog object for this release from the change fragments since the previous
# one, or from the commit titles when there are none.
function New-Changelog($PreviousManifest, [String]$Commit) {
    $previousVersion = $null
    $previousCommit = $null
    if ($PreviousManifest) {
        if ($PreviousManifest.version) { $previousVersion = [String]$PreviousManifest.version }
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

    $notes = @(Get-ReleaseNotes $previousCommit $range $Commit)
    $titles = if ($notes.Count -gt 0) { @() } else { Invoke-GitUtf8 log --no-merges --format=%s @range }

    # "форматирование" (formatting), "неиспользуемый" (unused code cleanup), test-only fixes
    # ("тест ...") and merge commits carry no user-visible change.
    $skipPattern = '(?i)^merge\s|' + [regex]::Unescape('форматировани') + '|' +
        [regex]::Unescape('неиспользуем') + '|:\s*' + [regex]::Unescape('тест') + '\s'
    # Documentation, CI and test commits are not changes of the app.
    $skipModules = @('WARD.md', 'README', 'README.md', 'AGENTS.md', 'FORK.md', 'CI', 'Docs', 'Tests')
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
        if ($skipModules -contains $module) { continue }
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
        image_url = @($notes | ForEach-Object { $_.image_url } | Where-Object { $_ }) | Select-Object -First 1
        notes = $notes
        groups = @($groups.Keys | ForEach-Object { [ordered]@{ module = $_; items = @($groups[$_]) } })
    }
}

$Commit = (git -C $RepoRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or -not $Commit) { throw 'Unable to determine the release commit' }

# The previous release, to list what changed since. Unreachable or missing manifests just
# mean there is no previous release to compare against.
$PreviousManifest = $null
if ($PreviousCommit) {
    $previousSha = (git -C $RepoRoot rev-parse --verify --quiet "$PreviousCommit^{commit}")
    if ($LASTEXITCODE -ne 0 -or -not $previousSha) { throw "Commit '$PreviousCommit' was not found" }
    $PreviousManifest = [pscustomobject]@{ version = $null; commit = $previousSha.Trim() }
    Write-Output "Previous release commit: $($PreviousManifest.commit)"
} else {
    try {
        $PreviousManifest = Invoke-RestMethod -Uri "$ReleasesUrl/stable/manifest.json?r=$([guid]::NewGuid().ToString('N'))" -TimeoutSec 30
        Write-Output "Previous release: $($PreviousManifest.version)"
    } catch {
        Write-Warning "No previous release manifest: $($_.Exception.Message)"
    }
}

$ChangelogPath = $null
$ChangelogUrl = $null
$ChangelogImages = [System.Collections.Generic.List[Object]]::new()
try {
    $Changelog = New-Changelog $PreviousManifest $Commit
    $ChangelogJson = $Changelog | ConvertTo-Json -Depth 6
    $ChangelogPath = Join-Path ([System.IO.Path]::GetTempPath()) "warp-fork-changelog-$Version.json"
    [System.IO.File]::WriteAllText($ChangelogPath, $ChangelogJson, [System.Text.UTF8Encoding]::new($false))
    $ChangelogUrl = "$ReleasesUrl/stable/$Version/changelog.json"
    if ($Changelog.notes.Count -gt 0) {
        Write-Output "Changelog: $($Changelog.notes.Count) notes with $($ChangelogImages.Count) images since $($Changelog.previous_version)"
    } else {
        $itemCount = ($Changelog.groups | ForEach-Object { $_.items.Count } | Measure-Object -Sum).Sum
        Write-Output "Changelog: no change fragments, $itemCount commits in $($Changelog.groups.Count) modules since $($Changelog.previous_version)"
    }
} catch {
    if ($ChangelogOnly) { throw }
    # A release without a changelog is still a valid release.
    Write-Warning "Changelog generation failed: $($_.Exception.Message)"
    $ChangelogPath = $null
    $ChangelogUrl = $null
    $ChangelogImages.Clear()
}

if ($ChangelogOnly) {
    [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
    foreach ($image in $ChangelogImages) { Write-Output "Image: $($image.Path) -> images/$($image.Name) ($($image.ContentType))" }
    Write-Output $ChangelogJson
    exit 0
}

$Sha256 = (Get-FileHash -LiteralPath $InstallerPath -Algorithm SHA256).Hash.ToLowerInvariant()
$InstallerUrl = "$ReleasesUrl/stable/$Version/$InstallerName"

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
# One part at a time: a runner with a slow uplink splits its bandwidth across concurrent parts,
# and a part that takes over a minute is cut by the ingress in front of the bucket (error 499).
$awsConfig = "[default]`ns3 =`n    addressing_style = path`n    max_concurrent_requests = 1`n    multipart_chunksize = 8MB`n"
[System.IO.File]::WriteAllText($env:AWS_CONFIG_FILE, $awsConfig)

try {
    $endpoint = $env:WARP_FORK_S3_ENDPOINT.TrimEnd('/')
    $target = "s3://$Bucket/$($Prefix.Trim('/'))/stable"
    # Upload the installer first so the manifest never points at a missing file.
    aws --endpoint-url $endpoint s3 cp --only-show-errors $InstallerPath "$target/$Version/$InstallerName" --content-type application/octet-stream
    if ($LASTEXITCODE -ne 0) { throw 'Uploading the installer failed' }
    if ($ChangelogPath) {
        # Pictures first so the changelog never points at a missing file.
        foreach ($image in $ChangelogImages) {
            aws --endpoint-url $endpoint s3 cp --only-show-errors $image.Path "$target/$Version/images/$($image.Name)" --content-type $image.ContentType
            if ($LASTEXITCODE -ne 0) { throw "Uploading the image $($image.Name) failed" }
        }
        aws --endpoint-url $endpoint s3 cp --only-show-errors $ChangelogPath "$target/$Version/changelog.json" --content-type 'application/json; charset=utf-8'
        if ($LASTEXITCODE -ne 0) { throw 'Uploading the changelog failed' }
    }
    aws --endpoint-url $endpoint s3 cp --only-show-errors $ManifestPath "$target/manifest.json" --content-type application/json --cache-control no-cache
    if ($LASTEXITCODE -ne 0) { throw 'Uploading the manifest failed' }
    # The manifest now points at this version, so older version folders are no longer needed.
    # A failed cleanup leaves extra files behind and does not fail the release.
    aws --endpoint-url $endpoint s3 rm --only-show-errors "$target/" --recursive --exclude 'manifest.json' --exclude "$Version/*"
    if ($LASTEXITCODE -ne 0) { Write-Warning 'Removing older versions failed' }
} finally {
    foreach ($name in 'AWS_ACCESS_KEY_ID', 'AWS_SECRET_ACCESS_KEY', 'AWS_CONFIG_FILE') {
        Remove-Item "Env:\$name" -ErrorAction SilentlyContinue
    }
}

Write-Output "Published $Version"
Write-Output "Manifest URL: $ReleasesUrl/stable/manifest.json"
