<#
.SYNOPSIS
    Captures a screenshot of a running window by process name.

.DESCRIPTION
    Development helper for verifying the desktop shell renders correctly without a human having to
    look at the screen. Brings the window to the foreground, waits briefly for the compositor to
    settle, then captures its client rectangle to a PNG.

.PARAMETER ProcessName
    Process whose main window to capture, e.g. "beastube-app".

.PARAMETER OutFile
    Path of the PNG to write.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ProcessName,
    [Parameter(Mandatory = $true)][string]$OutFile
)

$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

Add-Type @"
using System;
using System.Runtime.InteropServices;

public static class Win32Window {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }

    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);

    // GetWindowRect includes the drop shadow; DwmGetWindowAttribute with
    // DWMWA_EXTENDED_FRAME_BOUNDS (9) returns the visible frame, which is what a screenshot should
    // show.
    [DllImport("dwmapi.dll")]
    public static extern int DwmGetWindowAttribute(IntPtr hWnd, int attr, out RECT value, int size);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
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

# SW_RESTORE = 9, in case the window is minimized.
if ([Win32Window]::IsIconic($handle)) { [void][Win32Window]::ShowWindow($handle, 9) }
[void][Win32Window]::SetForegroundWindow($handle)
Start-Sleep -Milliseconds 900

$rect = New-Object Win32Window+RECT
# DWMWA_EXTENDED_FRAME_BOUNDS = 9
$size = [System.Runtime.InteropServices.Marshal]::SizeOf([type]'Win32Window+RECT')
if ([Win32Window]::DwmGetWindowAttribute($handle, 9, [ref]$rect, $size) -ne 0) {
    [void][Win32Window]::GetWindowRect($handle, [ref]$rect)
}

$width = $rect.Right - $rect.Left
$height = $rect.Bottom - $rect.Top
if ($width -le 0 -or $height -le 0) {
    Write-Error "window has no visible area ($width x $height)"
    exit 1
}

$bitmap = New-Object System.Drawing.Bitmap $width, $height
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size)
$graphics.Dispose()

$directory = Split-Path -Parent $OutFile
if ($directory -and -not (Test-Path $directory)) {
    New-Item -ItemType Directory -Path $directory -Force | Out-Null
}
$bitmap.Save($OutFile, [System.Drawing.Imaging.ImageFormat]::Png)
$bitmap.Dispose()

Write-Output "captured ${width}x${height} -> $OutFile"
