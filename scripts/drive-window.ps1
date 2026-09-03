<#
.SYNOPSIS
    Sends keystrokes to a running window, for smoke-testing the desktop app without a human.

.DESCRIPTION
    Development helper. Focuses the window, sends a key sequence, waits, and optionally captures a
    screenshot. Used to verify an end-to-end path (type a query, press Enter, see results) inside
    the real shell rather than in a browser.

.PARAMETER ProcessName
    Process whose main window to drive.

.PARAMETER Keys
    SendKeys sequence. See the .NET SendKeys documentation for the escaping rules; note that
    ^ % + ~ ( ) { } are special and must be braced to be sent literally.

.PARAMETER SettleSeconds
    How long to wait after sending, for the application to react.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ProcessName,
    [Parameter(Mandatory = $true)][string]$Keys,
    [int]$SettleSeconds = 4
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Focus {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);
}
"@

$process = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue |
    Where-Object { $_.MainWindowHandle -ne 0 } |
    Select-Object -First 1

if (-not $process) {
    Write-Error "no window found for process '$ProcessName'"
    exit 1
}

# SW_RESTORE = 9
if ([Focus]::IsIconic($process.MainWindowHandle)) {
    [void][Focus]::ShowWindow($process.MainWindowHandle, 9)
}
[void][Focus]::SetForegroundWindow($process.MainWindowHandle)

# The webview needs a moment to take focus before it will accept input.
Start-Sleep -Milliseconds 800

[System.Windows.Forms.SendKeys]::SendWait($Keys)
Start-Sleep -Seconds $SettleSeconds

Write-Output "sent to pid=$($process.Id): $Keys"
