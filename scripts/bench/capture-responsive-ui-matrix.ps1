<#
.SYNOPSIS
Launch target\debug\echo.exe with the responsive-matrix fixture, resize its main
window through a matrix of sizes and capture a PrintWindow screenshot of each,
for checking responsive layout by eye.
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputDir
)

$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class EchoResponsiveNative {
    public delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr lParam);

    [StructLayout(LayoutKind.Sequential)]
    public struct Rect { public int Left, Top, Right, Bottom; }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool EnumWindows(EnumWindowsProc callback, IntPtr parameter);

    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool IsWindowVisible(IntPtr hwnd);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool SetWindowPos(
        IntPtr hwnd, IntPtr insertAfter, int x, int y, int width, int height, uint flags);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool SetCursorPos(int x, int y);

    public static IntPtr FindLargestVisibleWindow(uint targetProcessId) {
        IntPtr found = IntPtr.Zero;
        long largestArea = 0;
        EnumWindows((candidate, unused) => {
            uint processId;
            GetWindowThreadProcessId(candidate, out processId);
            if (processId != targetProcessId || !IsWindowVisible(candidate)) return true;

            Rect rect;
            if (!GetWindowRect(candidate, out rect)) return true;
            long area = (long)(rect.Right - rect.Left) * (rect.Bottom - rect.Top);
            if (area > largestArea) {
                largestArea = area;
                found = candidate;
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
'@

$resolvedOutput = [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $OutputDir))
New-Item -ItemType Directory -Force $resolvedOutput | Out-Null

$env:ECHO_BENCH_UI_FIXTURE = $null
$env:ECHO_BENCH_FEEDBACK_UI = $null
$env:ECHO_BENCH_RESPONSIVE_MATRIX = '1'
$env:ECHO_LOG_FILE = '1'
$env:ECHO_LOG_FILE_PATH = Join-Path $resolvedOutput 'echo-responsive.log'

$executable = Resolve-Path 'target\debug\echo.exe'
$process = Start-Process -FilePath $executable -PassThru

try {
    $hwnd = [IntPtr]::Zero
    $deadline = (Get-Date).AddSeconds(30)
    while ($hwnd -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 250
        $hwnd = [EchoResponsiveNative]::FindLargestVisibleWindow($process.Id)
    }
    if ($hwnd -eq [IntPtr]::Zero) {
        throw 'The owned responsive-fixture window was not found.'
    }

    Start-Sleep -Seconds 8
    $hwnd = [EchoResponsiveNative]::FindLargestVisibleWindow($process.Id)
    if ($hwnd -eq [IntPtr]::Zero) {
        throw 'The owned responsive-fixture window disappeared during startup.'
    }

    $original = New-Object EchoResponsiveNative+Rect
    [void][EchoResponsiveNative]::GetWindowRect($hwnd, [ref]$original)
    $baseWidth = $original.Right - $original.Left
    $baseHeight = $original.Bottom - $original.Top
    $cases = @(
        @{ Name = 'current'; Width = $baseWidth; Height = $baseHeight },
        @{ Name = 'half-width'; Width = [math]::Max(1, [math]::Round($baseWidth / 2)); Height = $baseHeight },
        @{ Name = 'half-height'; Width = $baseWidth; Height = [math]::Max(1, [math]::Round($baseHeight / 2)) },
        @{ Name = 'half-width-height'; Width = [math]::Max(1, [math]::Round($baseWidth / 2)); Height = [math]::Max(1, [math]::Round($baseHeight / 2)) },
        @{ Name = 'double-width'; Width = $baseWidth * 2; Height = $baseHeight }
    )

    $observations = foreach ($case in $cases) {
        [void][EchoResponsiveNative]::SetWindowPos(
            $hwnd, [IntPtr]::Zero, $original.Left, $original.Top,
            $case.Width, $case.Height, 0x0014)
        Start-Sleep -Seconds 3

        $actual = New-Object EchoResponsiveNative+Rect
        [void][EchoResponsiveNative]::GetWindowRect($hwnd, [ref]$actual)
        [void][EchoResponsiveNative]::SetCursorPos(
            [math]::Max(0, $actual.Left - 20), [math]::Max(0, $actual.Top - 20))
        Start-Sleep -Milliseconds 500

        $width = $actual.Right - $actual.Left
        $height = $actual.Bottom - $actual.Top
        if ($width -lt 1 -or $height -lt 1) {
            throw "Invalid captured bounds for $($case.Name): ${width}x${height}"
        }

        $bitmap = New-Object Drawing.Bitmap($width, $height)
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        $hdc = $graphics.GetHdc()
        try {
            if (-not [EchoResponsiveNative]::PrintWindow($hwnd, $hdc, 2)) {
                throw "PrintWindow failed for $($case.Name)."
            }
        } finally {
            $graphics.ReleaseHdc($hdc)
            $graphics.Dispose()
        }

        $path = Join-Path $resolvedOutput ("{0}-{1}x{2}.png" -f $case.Name, $width, $height)
        $bitmap.Save($path, [Drawing.Imaging.ImageFormat]::Png)
        $bitmap.Dispose()

        [pscustomobject]@{
            name = $case.Name
            requested_width = $case.Width
            requested_height = $case.Height
            actual_width = $width
            actual_height = $height
            screenshot = Split-Path $path -Leaf
        }
    }

    $observations | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $resolvedOutput 'matrix.json')
    [void][EchoResponsiveNative]::SetWindowPos(
        $hwnd, [IntPtr]::Zero, $original.Left, $original.Top,
        $baseWidth, $baseHeight, 0x0014)
} finally {
    if (-not $process.HasExited) {
        $process.CloseMainWindow() | Out-Null
        if (-not $process.WaitForExit(10000)) {
            Stop-Process -Id $process.Id
        }
    }
}
