<#
.SYNOPSIS
    Rests the pointer on a point inside a running window, for smoke-testing hover behaviour.

.DESCRIPTION
    Development helper. Some behaviour only exists under a resting pointer — the card preview, for
    one — and a click helper cannot exercise it because a click moves on. This moves the pointer
    there and holds it, so a screenshot taken afterwards shows the hovered state.

    Coordinates are relative to the same origin capture-window.ps1 uses, so a point read off a
    screenshot can be hovered directly.

.PARAMETER ProcessName
    Process whose main window to hover in.

.PARAMETER X
    X, in pixels from the window's left edge.

.PARAMETER Y
    Y, in pixels from the window's top edge.

.PARAMETER HoldSeconds
    How long to rest the pointer there. Must exceed whatever delay the behaviour waits for.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ProcessName,
    [Parameter(Mandatory = $true)][int]$X,
    [Parameter(Mandatory = $true)][int]$Y,
    [int]$HoldSeconds = 4
)

$ErrorActionPreference = 'Stop'

Add-Type @"
using System;
using System.Runtime.InteropServices;

[StructLayout(LayoutKind.Sequential)]
public struct HoverPoint {
    public int X;
    public int Y;
}

public static class Hover {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr hWnd, ref HoverPoint point);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
}
"@

$process = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue |
    Where-Object { $_.MainWindowHandle -ne 0 } |
    Select-Object -First 1

if (-not $process) {
    Write-Error "no window found for process '$ProcessName'"
    exit 1
}

$handle = $process.MainWindowHandle

# SW_RESTORE = 9
if ([Hover]::IsIconic($handle)) {
    [void][Hover]::ShowWindow($handle, 9)
}
[void][Hover]::SetForegroundWindow($handle)
Start-Sleep -Milliseconds 400

$point = New-Object HoverPoint
$point.X = $X
$point.Y = $Y
[void][Hover]::ClientToScreen($handle, [ref]$point)

# Approach from a nearby point first: an instantaneous jump can be delivered as a single move that
# some hit-testing treats as never having entered the element.
[void][Hover]::SetCursorPos($point.X - 20, $point.Y - 20)
Start-Sleep -Milliseconds 150
[void][Hover]::SetCursorPos($point.X, $point.Y)

Start-Sleep -Seconds $HoldSeconds
Write-Output "hovered client($X,$Y) -> screen($($point.X),$($point.Y)) for ${HoldSeconds}s"
