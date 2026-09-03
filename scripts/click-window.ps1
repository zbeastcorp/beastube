<#
.SYNOPSIS
    Clicks a point inside a running window, for smoke-testing the desktop app without a human.

.DESCRIPTION
    Development helper, the pointer counterpart to drive-window.ps1. Coordinates are relative to
    the window's *client* area — the same origin capture-window.ps1 uses — so a point read off a
    screenshot can be clicked directly without accounting for the title bar or window position.

.PARAMETER ProcessName
    Process whose main window to click in.

.PARAMETER X
    Client-area X, in pixels from the left edge.

.PARAMETER Y
    Client-area Y, in pixels from the top edge.

.PARAMETER SettleSeconds
    How long to wait after clicking, for the application to react.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ProcessName,
    [Parameter(Mandatory = $true)][int]$X,
    [Parameter(Mandatory = $true)][int]$Y,
    [int]$SettleSeconds = 3
)

$ErrorActionPreference = 'Stop'

Add-Type @"
using System;
using System.Runtime.InteropServices;

// Declared here rather than using System.Drawing.Point: on .NET Core that type lives in
// System.Drawing.Primitives, which Add-Type does not reference by default.
[StructLayout(LayoutKind.Sequential)]
public struct ClientPoint {
    public int X;
    public int Y;
}

public static class Click {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr hWnd, ref ClientPoint point);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);

    public const uint LEFTDOWN = 0x0002;
    public const uint LEFTUP   = 0x0004;
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
if ([Click]::IsIconic($handle)) {
    [void][Click]::ShowWindow($handle, 9)
}
[void][Click]::SetForegroundWindow($handle)
Start-Sleep -Milliseconds 500

$point = New-Object ClientPoint
$point.X = $X
$point.Y = $Y
[void][Click]::ClientToScreen($handle, [ref]$point)

[void][Click]::SetCursorPos($point.X, $point.Y)
Start-Sleep -Milliseconds 120
[Click]::mouse_event([Click]::LEFTDOWN, 0, 0, 0, [UIntPtr]::Zero)
[Click]::mouse_event([Click]::LEFTUP, 0, 0, 0, [UIntPtr]::Zero)

Start-Sleep -Seconds $SettleSeconds
Write-Output "clicked client($X,$Y) -> screen($($point.X),$($point.Y)) in pid=$($process.Id)"
