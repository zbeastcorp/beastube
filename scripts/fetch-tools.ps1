<#
.SYNOPSIS
    Fetches the third-party tools BEASTUBE bundles: yt-dlp and ffmpeg.

.DESCRIPTION
    Downloads both executables into `src-tauri/binaries/`, from where `tauri.conf.json` picks them
    up as bundle resources so the installer puts them beside the application. That is what makes a
    download work on a machine the user has not prepared: `locate()` searches the application's own
    directories before `PATH`, so the bundled copies are found first.

    Every download is verified against a checksum the vendor publishes separately from the artifact.
    A mismatch fails the build rather than shipping an executable we cannot account for — these are
    programs the installer will place on someone else's computer, so "it downloaded fine" is not a
    sufficient standard.

    Nothing here runs the tools. They are fetched, verified and placed; the application decides when
    to invoke them.

    Idempotent: an existing file whose checksum already matches is left alone, so a repeat build is
    a hash check rather than a re-download.

.PARAMETER Force
    Re-download even when the local copy already matches.

.PARAMETER SkipFfmpeg
    Fetch only yt-dlp. Useful on a slow connection; the application reports a missing muxer
    honestly, so a build without it is degraded rather than broken.
#>
[CmdletBinding()]
param(
    [switch]$Force,
    [switch]$SkipFfmpeg
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'   # The progress UI makes Invoke-WebRequest an order of magnitude slower.

$repoRoot = Split-Path -Parent $PSScriptRoot
$outDir = Join-Path $repoRoot 'src-tauri/binaries'
$licenseDir = Join-Path $outDir 'licenses'

# Pinned rather than "latest": a build that silently changes what it ships is not reproducible, and
# a yt-dlp release that breaks extraction should be a deliberate bump with a note, not a surprise
# in whatever build ran next.
$ytDlpVersion = '2026.08.19'
$ytDlpBase = "https://github.com/yt-dlp/yt-dlp/releases/download/$ytDlpVersion"

# gyan.dev is the Windows build ffmpeg.org itself links to, and it publishes a detached .sha256.
# The "essentials" build is the small one: it carries the demuxers, decoders and the mp4 muxer a
# download needs, and leaves out the encoders we never invoke.
$ffmpegUrl = 'https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip'
$ffmpegSumUrl = "$ffmpegUrl.sha256"

New-Item -ItemType Directory -Force -Path $outDir | Out-Null
New-Item -ItemType Directory -Force -Path $licenseDir | Out-Null

function Get-Sha256([string]$Path) {
    (Get-FileHash -Path $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

<#
    Downloads $Url to $Destination unless it is already there with the right hash.
    $ExpectedHash is lower-case hex SHA-256.
#>
function Save-Verified {
    param(
        [Parameter(Mandatory)][string]$Url,
        [Parameter(Mandatory)][string]$Destination,
        [Parameter(Mandatory)][string]$ExpectedHash,
        [string]$Label = (Split-Path -Leaf $Destination)
    )

    if (-not $Force -and (Test-Path $Destination) -and (Get-Sha256 $Destination) -eq $ExpectedHash) {
        Write-Host "  $Label is already present and verified"
        return
    }

    # Downloaded to a temporary name and only moved into place once the hash matches, so an
    # interrupted or corrupted transfer can never leave a half-file that looks installed.
    $temp = "$Destination.partial"
    Write-Host "  downloading $Label..."
    Invoke-WebRequest -Uri $Url -OutFile $temp -UseBasicParsing

    $actual = Get-Sha256 $temp
    if ($actual -ne $ExpectedHash) {
        Remove-Item $temp -Force -ErrorAction SilentlyContinue
        throw "$Label failed its checksum: expected $ExpectedHash, got $actual"
    }

    Move-Item -Path $temp -Destination $Destination -Force
    $size = [math]::Round((Get-Item $Destination).Length / 1MB, 1)
    Write-Host "  $Label verified (${size} MB)"
}

# --- yt-dlp -----------------------------------------------------------------------------------
# The release publishes SHA2-256SUMS as a separate file. Reading the expected hash from there, and
# then checking the artifact against it, is what makes this a verification rather than a download.
Write-Host "yt-dlp $ytDlpVersion"
$sumsPath = Join-Path $env:TEMP "yt-dlp-$ytDlpVersion-SHA2-256SUMS"
Invoke-WebRequest -Uri "$ytDlpBase/SHA2-256SUMS" -OutFile $sumsPath -UseBasicParsing

$ytDlpLine = Get-Content $sumsPath | Where-Object { $_ -match '\s+yt-dlp\.exe$' } | Select-Object -First 1
if (-not $ytDlpLine) { throw 'the yt-dlp release does not list a checksum for yt-dlp.exe' }
$ytDlpHash = ($ytDlpLine -split '\s+')[0].ToLowerInvariant()

Save-Verified -Url "$ytDlpBase/yt-dlp.exe" `
    -Destination (Join-Path $outDir 'yt-dlp.exe') `
    -ExpectedHash $ytDlpHash -Label 'yt-dlp.exe'

Invoke-WebRequest -Uri 'https://raw.githubusercontent.com/yt-dlp/yt-dlp/master/LICENSE' `
    -OutFile (Join-Path $licenseDir 'yt-dlp-LICENSE.txt') -UseBasicParsing

# --- ffmpeg -----------------------------------------------------------------------------------
if ($SkipFfmpeg) {
    Write-Host 'ffmpeg skipped at your request; downloads will be unavailable in this build'
    exit 0
}

Write-Host 'ffmpeg (release essentials)'
# The sidecar is `<hash> *<filename>`; only the hash is wanted.
$ffmpegHash = ((Invoke-WebRequest -Uri $ffmpegSumUrl -UseBasicParsing).Content -split '\s+')[0].ToLowerInvariant()
$zipPath = Join-Path $env:TEMP 'ffmpeg-release-essentials.zip'
Save-Verified -Url $ffmpegUrl -Destination $zipPath -ExpectedHash $ffmpegHash -Label 'ffmpeg archive'

# Extracted to a scratch directory and only the one executable kept: the archive carries ffplay,
# ffprobe, documentation and presets, none of which this application invokes.
$extractDir = Join-Path $env:TEMP 'beastube-ffmpeg-extract'
Remove-Item $extractDir -Recurse -Force -ErrorAction SilentlyContinue
Expand-Archive -Path $zipPath -DestinationPath $extractDir -Force

$ffmpegExe = Get-ChildItem -Path $extractDir -Filter 'ffmpeg.exe' -Recurse | Select-Object -First 1
if (-not $ffmpegExe) { throw 'the ffmpeg archive did not contain ffmpeg.exe' }
Copy-Item -Path $ffmpegExe.FullName -Destination (Join-Path $outDir 'ffmpeg.exe') -Force

$ffmpegLicense = Get-ChildItem -Path $extractDir -Filter 'LICENSE*' -Recurse | Select-Object -First 1
if ($ffmpegLicense) {
    Copy-Item -Path $ffmpegLicense.FullName -Destination (Join-Path $licenseDir 'ffmpeg-LICENSE.txt') -Force
}
Remove-Item $extractDir -Recurse -Force -ErrorAction SilentlyContinue

$size = [math]::Round((Get-Item (Join-Path $outDir 'ffmpeg.exe')).Length / 1MB, 1)
Write-Host "  ffmpeg.exe extracted (${size} MB)"
Write-Host "Tools are in $outDir and will be bundled by the installer."
