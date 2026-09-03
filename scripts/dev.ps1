<#
.SYNOPSIS
    Starts the BEASTUBE desktop app in development, cleaning up any previous run first.

.DESCRIPTION
    `tauri dev` starts its own Vite server on a fixed port and fails outright if that port is
    taken — which it routinely is after an earlier run was killed without its child surviving.
    This stops the old app and frees the port before starting, so a relaunch is one command rather
    than a hunt for stray processes.

.PARAMETER Wait
    Block until the app window appears, then report it. Without this the script returns as soon as
    the build starts.
#>
[CmdletBinding()]
param(
    [switch]$Wait
)

$ErrorActionPreference = 'Stop'
$devPort = 1420
$repoRoot = Split-Path -Parent $PSScriptRoot

Write-Host 'Stopping any previous run...'
Get-Process -Name 'beastube-app' -ErrorAction SilentlyContinue |
    ForEach-Object {
        Write-Host "  stopping beastube-app ($($_.Id))"
        Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
    }

Get-NetTCPConnection -LocalPort $devPort -State Listen -ErrorAction SilentlyContinue |
    ForEach-Object {
        $owner = Get-Process -Id $_.OwningProcess -ErrorAction SilentlyContinue
        if ($owner) {
            Write-Host "  freeing port $devPort held by $($owner.ProcessName) ($($owner.Id))"
            Stop-Process -Id $owner.Id -Force -ErrorAction SilentlyContinue
        }
    }

# Give the OS a moment to release the socket; a TIME_WAIT socket still fails strictPort binding.
$deadline = (Get-Date).AddSeconds(10)
while ((Get-NetTCPConnection -LocalPort $devPort -State Listen -ErrorAction SilentlyContinue) -and
       ((Get-Date) -lt $deadline)) {
    Start-Sleep -Milliseconds 250
}

if (Get-NetTCPConnection -LocalPort $devPort -State Listen -ErrorAction SilentlyContinue) {
    Write-Error "port $devPort is still in use; stop whatever holds it and retry"
    exit 1
}

Write-Host "Starting the desktop app (port $devPort free)..."
Push-Location $repoRoot
try {
    if ($Wait) {
        # `pnpm` on Windows is a .cmd shim, which Start-Process cannot launch directly.
        $job = Start-Process -FilePath $env:ComSpec -ArgumentList '/c', 'pnpm', 'tauri', 'dev' `
            -PassThru -NoNewWindow
        $deadline = (Get-Date).AddMinutes(10)
        while ((Get-Date) -lt $deadline) {
            $app = Get-Process -Name 'beastube-app' -ErrorAction SilentlyContinue |
                Where-Object { $_.MainWindowHandle -ne 0 } |
                Select-Object -First 1
            if ($app) {
                Write-Output "window up: pid=$($app.Id) title='$($app.MainWindowTitle)'"
                exit 0
            }
            if ($job.HasExited) {
                Write-Error 'the dev command exited before a window appeared'
                exit 1
            }
            Start-Sleep -Seconds 2
        }
        Write-Error 'timed out waiting for the window'
        exit 1
    }
    else {
        & cmd.exe /c pnpm tauri dev
    }
}
finally {
    Pop-Location
}
