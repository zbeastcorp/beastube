<#
.SYNOPSIS
    Produces the shareable copy of BEASTUBE, ready to push to GitHub.

.DESCRIPTION
    The working copy and the public copy are not the same thing. This directory accumulates things
    that must not be published — the fetched ffmpeg and yt-dlp binaries, build output, logs,
    downloaded test videos, and the updater's private signing key. The public copy is source only.

    Rather than maintain a second folder by hand and let the two drift, this generates it from
    git's own view of the project:

        git ls-files --cached --others --exclude-standard

    which is every file git tracks *plus* every new file that is not ignored — precisely the set
    that would end up in a commit. Anything `.gitignore` excludes is excluded here by construction,
    so a new kind of build artefact can never leak by being forgotten: ignoring it for git ignores
    it for the export too.

    The export is a plain directory, not a git repository, so pushing it is a deliberate act rather
    than something this script can do by accident.

.PARAMETER Destination
    Where to write the copy. Defaults to `BEASTUBE-github` beside the project.

.PARAMETER Force
    Replace an existing destination. Without it, a non-empty destination is an error rather than a
    silent overwrite.

.EXAMPLE
    pnpm export:github
#>
[CmdletBinding()]
param(
    [string]$Destination,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not $Destination) {
    $Destination = Join-Path (Split-Path -Parent $repoRoot) 'BEASTUBE-github'
}

Push-Location $repoRoot
try {
    # Guard against exporting something that is not a git working tree, which would silently
    # produce an empty folder.
    $null = git rev-parse --is-inside-work-tree 2>$null
    if ($LASTEXITCODE -ne 0) {
        throw "Not a git repository: $repoRoot. The export is built from git's file list."
    }

    if (Test-Path $Destination) {
        $existing = @(Get-ChildItem -LiteralPath $Destination -Force -ErrorAction SilentlyContinue)
        if ($existing.Count -gt 0 -and -not $Force) {
            throw "$Destination already exists and is not empty. Re-run with -Force to replace it."
        }
        if ($Force) {
            Remove-Item -LiteralPath $Destination -Recurse -Force
        }
    }
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null

    # Tracked files and new-but-not-ignored files: exactly what a commit would carry.
    $files = git ls-files --cached --others --exclude-standard
    if ($LASTEXITCODE -ne 0) { throw 'git ls-files failed.' }

    $copied = 0
    $skipped = New-Object System.Collections.Generic.List[string]
    $private = New-Object System.Collections.Generic.List[string]

    # Paths that are part of the work but not part of the product.
    #
    # `docs/research/` is the reading that went into the design — provider internals, WebView2
    # behaviour, crate comparisons. It is genuinely useful and it is also a working notebook:
    # it quotes absolute paths from the machine it was written on, so publishing it would publish
    # a developer's username and directory layout for no benefit to anyone reading the project.
    # The decisions those notes led to are in `docs/architecture-decisions/`, which does ship.
    $privatePaths = @(
        'docs/research/'
    )

    # The name to scan file contents for, so a stray local path cannot slip through in a file
    # nobody thought to check. Compared case-insensitively.
    $localUser = $env:USERNAME

    foreach ($relative in $files) {
        if ([string]::IsNullOrWhiteSpace($relative)) { continue }

        # A belt-and-braces refusal for anything that must never be published, even if someone
        # loosens .gitignore. A signing key leaking is not a recoverable mistake.
        if ($relative -match '\.(key|pem)$' -or $relative -match '(^|/)\.env') {
            $skipped.Add($relative)
            continue
        }

        if ($privatePaths | Where-Object { $relative.StartsWith($_) }) {
            $private.Add($relative)
            continue
        }

        $source = Join-Path $repoRoot $relative
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { continue }

        $target = Join-Path $Destination $relative
        $targetDir = Split-Path -Parent $target
        if ($targetDir -and -not (Test-Path -LiteralPath $targetDir)) {
            New-Item -ItemType Directory -Path $targetDir -Force | Out-Null
        }
        Copy-Item -LiteralPath $source -Destination $target -Force
        $copied++
    }

    # One last read over what is about to be published, looking for the local account name.
    #
    # Everything above works on paths; this works on contents, which is where a leak actually hides
    # — a pasted stack trace, a quoted file path in a comment, a note that says where something was
    # measured. Text files only, and bounded, so this stays a few seconds rather than a scan.
    $leaked = New-Object System.Collections.Generic.List[string]
    if ($localUser) {
        $textLike = '\.(md|txt|json|jsonc|toml|ya?ml|ts|tsx|js|mjs|cjs|rs|css|html|ps1|sql)$'
        Get-ChildItem -LiteralPath $Destination -Recurse -File |
            Where-Object { $_.Name -match $textLike } |
            ForEach-Object {
                $content = Get-Content -LiteralPath $_.FullName -Raw -ErrorAction SilentlyContinue
                if ($content -and $content -match [regex]::Escape($localUser)) {
                    $leaked.Add($_.FullName.Substring($Destination.Length + 1))
                }
            }
    }

    $bytes = (Get-ChildItem -LiteralPath $Destination -Recurse -File | Measure-Object Length -Sum).Sum

    Write-Host ''
    Write-Host "Shareable copy written to $Destination"
    Write-Host ("  {0} files, {1:N1} MB" -f $copied, ($bytes / 1MB))
    if ($skipped.Count -gt 0) {
        Write-Host "  refused to copy (secrets):" -ForegroundColor Yellow
        $skipped | ForEach-Object { Write-Host "    $_" -ForegroundColor Yellow }
    }
    if ($private.Count -gt 0) {
        Write-Host ("  held back as working material: {0} file(s)" -f $private.Count) -ForegroundColor DarkGray
        $privatePaths | ForEach-Object { Write-Host "    $_" -ForegroundColor DarkGray }
    }
    if ($leaked.Count -gt 0) {
        Write-Host ''
        Write-Host "  WARNING: '$localUser' appears in the exported files below." -ForegroundColor Red
        $leaked | ForEach-Object { Write-Host "    $_" -ForegroundColor Red }
        Write-Host '  Check these before pushing: a local path in a public repository names you.' -ForegroundColor Red
    }
    Write-Host ''
    Write-Host 'It contains source only: no binaries, no build output, no keys.'
    Write-Host 'To publish it:'
    Write-Host "  cd `"$Destination`""
    Write-Host '  git init && git add . && git commit -m "Initial commit"'
    Write-Host '  git remote add origin <your repository URL>'
    Write-Host '  git push -u origin main'
}
finally {
    Pop-Location
}
