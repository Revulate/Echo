<#
.SYNOPSIS
Run Echo against live Twitch chat without sending anything, sample the process,
capture screenshots and optionally drive UI fixture checkpoints, then check the
log with profile-log-check.
#>
[CmdletBinding()]
param(
    [string]$LaunchPath = "target\release\echo.exe",
    [string]$OutputRoot = "",

    [ValidateRange(60, 86400)]
    [int]$DurationSeconds = 600,

    [ValidateRange(100, 60000)]
    [int]$IntervalMilliseconds = 1000,

    [ValidateRange(5, 120)]
    [int]$ScreenshotDelaySeconds = 8,

    [string]$Scenario = "live UI no-send suite",
    [string]$MetricName = "echo-live-ui-no-send",
    [string]$ProfileLogCheckPath = "target\release\profile-log-check.exe",

    [string]$EmotePickerSweepChannel = "ExampleChannel",

    [switch]$UiFixtureInteractions,
    [switch]$UiFixtureSettingsOnly,
    [switch]$FeedbackUiCheckpoints,
    [switch]$CursorStabilityCheckpoints,
    [switch]$PopupMonitorCheckpoints,
    [switch]$LinkPreviewMatrix,
    [string]$LinkPreviewMatrixChannel = "examplechannel",

    [ValidateRange(0, 100000)]
    [int]$MinimumEmotePickerCatalogSize = 4000
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"
Import-Module (Join-Path $PSScriptRoot 'BenchLib.psm1') -Force -DisableNameChecking

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

public static class EchoLiveUiNativeMethods
{
    public delegate bool EnumWindowsProc(IntPtr window, IntPtr parameter);

    [StructLayout(LayoutKind.Sequential)]
    public struct Rect
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool GetWindowRect(IntPtr window, out Rect rect);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool PrintWindow(IntPtr window, IntPtr targetDeviceContext, uint flags);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool EnumWindows(EnumWindowsProc callback, IntPtr parameter);

    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);

    [DllImport("kernel32.dll")]
    public static extern uint GetCurrentThreadId();

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool AttachThreadInput(uint attach, uint attachTo, bool shouldAttach);

    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    public static extern IntPtr SetFocus(IntPtr window);

    [DllImport("user32.dll")]
    public static extern IntPtr GetFocus();

    [DllImport("user32.dll")]
    public static extern short GetKeyState(int virtualKey);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool BringWindowToTop(IntPtr window);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern int GetWindowText(IntPtr window, System.Text.StringBuilder text, int maxCount);

    [DllImport("user32.dll")]
    public static extern int GetWindowTextLength(IntPtr window);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool IsWindowVisible(IntPtr window);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool IsWindow(IntPtr window);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);

    [StructLayout(LayoutKind.Sequential)]
    public struct Point
    {
        public int X;
        public int Y;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct CursorInfo
    {
        public int Size;
        public int Flags;
        public IntPtr Cursor;
        public Point ScreenPosition;
    }

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool GetCursorInfo(ref CursorInfo cursorInfo);

    [DllImport("user32.dll", EntryPoint = "LoadCursorW", SetLastError = true)]
    public static extern IntPtr LoadCursor(IntPtr instance, IntPtr cursorName);

    [DllImport("user32.dll")]
    public static extern IntPtr SetCursor(IntPtr cursor);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool GetClientRect(IntPtr window, out Rect rect);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool ClientToScreen(IntPtr window, ref Point point);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool SetCursorPos(int x, int y);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool SetForegroundWindow(IntPtr window);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr CreateWindowEx(
        uint extendedStyle,
        string className,
        string windowName,
        uint style,
        int x,
        int y,
        int width,
        int height,
        IntPtr parent,
        IntPtr menu,
        IntPtr instance,
        IntPtr parameter
    );

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool DestroyWindow(IntPtr window);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool ShowWindow(IntPtr window, int command);

    [DllImport("user32.dll")]
    public static extern IntPtr WindowFromPoint(Point point);

    [DllImport("user32.dll")]
    public static extern IntPtr GetAncestor(IntPtr window, uint flags);

    [DllImport("user32.dll")]
    public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);

    [StructLayout(LayoutKind.Sequential)]
    public struct KeyboardInput
    {
        public ushort VirtualKey;
        public ushort ScanCode;
        public uint Flags;
        public uint Time;
        public UIntPtr ExtraInfo;
    }

    // INPUT's native union is sized by MOUSEINPUT (32 bytes on x64), even
    // when this harness only populates the smaller KEYBDINPUT member.
    [StructLayout(LayoutKind.Explicit, Size = 32)]
    public struct InputUnion
    {
        [FieldOffset(0)]
        public KeyboardInput Keyboard;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct Input
    {
        public uint Type;
        public InputUnion Data;
    }

    [DllImport("user32.dll", SetLastError = true)]
    public static extern uint SendInput(uint inputCount, Input[] inputs, int inputSize);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool SetWindowPos(
        IntPtr window,
        IntPtr insertAfter,
        int x,
        int y,
        int width,
        int height,
        uint flags
    );

    private static Input KeyInput(ushort virtualKey, uint flags)
    {
        return new Input
        {
            Type = 1,
            Data = new InputUnion
            {
                Keyboard = new KeyboardInput
                {
                    VirtualKey = virtualKey,
                    Flags = flags
                }
            }
        };
    }

    private static bool WaitForKeyState(ushort virtualKey, bool pressed)
    {
        for (int attempt = 0; attempt < 50; attempt++)
        {
            if ((GetKeyState(virtualKey) < 0) == pressed)
            {
                return true;
            }
            System.Threading.Thread.Sleep(5);
        }
        return false;
    }

    public static uint SendTabToWindow(IntPtr window, bool reverse)
    {
        if (!IsWindow(window))
        {
            throw new InvalidOperationException("Echo's main window is no longer valid.");
        }

        const uint GetAncestorRoot = 2;
        const ushort VirtualKeyShift = 0x10;
        const ushort VirtualKeyTab = 0x09;
        const uint KeyEventKeyUp = 0x0002;

        IntPtr targetRoot = GetAncestor(window, GetAncestorRoot);
        if (targetRoot == IntPtr.Zero)
        {
            targetRoot = window;
        }

        uint currentThread = GetCurrentThreadId();
        uint ignoredProcess;
        uint targetThread = GetWindowThreadProcessId(targetRoot, out ignoredProcess);
        if (targetThread == 0)
        {
            throw new InvalidOperationException("Could not resolve Echo's native window thread.");
        }
        IntPtr previousForeground = GetForegroundWindow();
        uint foregroundThread = previousForeground == IntPtr.Zero
            ? 0
            : GetWindowThreadProcessId(previousForeground, out ignoredProcess);
        bool attachedTarget = false;
        bool attachedForeground = false;
        bool injectedShiftHeld = false;
        uint sent = 0;

        try
        {
            if (foregroundThread != 0 &&
                foregroundThread != currentThread &&
                foregroundThread != targetThread)
            {
                attachedForeground = AttachThreadInput(currentThread, foregroundThread, true);
                if (!attachedForeground)
                {
                    throw new InvalidOperationException(
                        "Could not attach the harness to the foreground input queue. Win32 error " +
                        Marshal.GetLastWin32Error() + "."
                    );
                }
            }

            if (targetThread != 0 && targetThread != currentThread)
            {
                attachedTarget = AttachThreadInput(currentThread, targetThread, true);
                if (!attachedTarget)
                {
                    throw new InvalidOperationException(
                        "Could not attach the harness to Echo's input queue. Win32 error " +
                        Marshal.GetLastWin32Error() + "."
                    );
                }
            }

            BringWindowToTop(targetRoot);
            SetForegroundWindow(targetRoot);
            SetFocus(targetRoot);

            IntPtr foregroundRoot = GetForegroundWindow();
            if (foregroundRoot != IntPtr.Zero)
            {
                IntPtr resolvedForegroundRoot = GetAncestor(foregroundRoot, GetAncestorRoot);
                if (resolvedForegroundRoot != IntPtr.Zero)
                {
                    foregroundRoot = resolvedForegroundRoot;
                }
            }

            IntPtr focusedRoot = GetFocus();
            if (focusedRoot != IntPtr.Zero)
            {
                IntPtr resolvedFocusedRoot = GetAncestor(focusedRoot, GetAncestorRoot);
                if (resolvedFocusedRoot != IntPtr.Zero)
                {
                    focusedRoot = resolvedFocusedRoot;
                }
            }

            if (foregroundRoot != targetRoot || focusedRoot != targetRoot)
            {
                throw new InvalidOperationException(
                    "Echo did not own native foreground and keyboard focus before Tab injection."
                );
            }

            int inputSize = Marshal.SizeOf(typeof(Input));
            if (reverse)
            {
                Input[] shiftDown = new[] { KeyInput(VirtualKeyShift, 0) };
                uint shiftDownSent = SendInput(1, shiftDown, inputSize);
                if (shiftDownSent != 1)
                {
                    throw new InvalidOperationException(
                        "SendInput could not press Shift. Win32 error " +
                        Marshal.GetLastWin32Error() + "."
                    );
                }
                sent += shiftDownSent;
                injectedShiftHeld = true;
                if (!WaitForKeyState(VirtualKeyShift, true))
                {
                    throw new InvalidOperationException(
                        "Echo's attached input queue did not observe Shift pressed before Tab."
                    );
                }

                Input[] tabInputs = new[]
                {
                    KeyInput(VirtualKeyTab, 0),
                    KeyInput(VirtualKeyTab, KeyEventKeyUp)
                };
                uint tabSent = SendInput(2, tabInputs, inputSize);
                if (tabSent != 2)
                {
                    throw new InvalidOperationException(
                        "SendInput accepted " + tabSent +
                        " of 2 Shift+Tab events. Win32 error " +
                        Marshal.GetLastWin32Error() + "."
                    );
                }
                sent += tabSent;

                // GPUI reads modifiers through GetKeyState while dequeuing the
                // Tab message. Keep Shift physically held until that separate
                // process has had time to consume the native key-down.
                System.Threading.Thread.Sleep(75);
                Input[] shiftUp = new[] { KeyInput(VirtualKeyShift, KeyEventKeyUp) };
                uint shiftUpSent = SendInput(1, shiftUp, inputSize);
                if (shiftUpSent != 1)
                {
                    throw new InvalidOperationException(
                        "SendInput could not release Shift. Win32 error " +
                        Marshal.GetLastWin32Error() + "."
                    );
                }
                sent += shiftUpSent;
                injectedShiftHeld = false;
            }
            else
            {
                Input[] tabInputs = new[]
                {
                    KeyInput(VirtualKeyTab, 0),
                    KeyInput(VirtualKeyTab, KeyEventKeyUp)
                };
                sent = SendInput(2, tabInputs, inputSize);
                if (sent != 2)
                {
                    throw new InvalidOperationException(
                        "SendInput accepted " + sent +
                        " of 2 Tab events. Win32 error " +
                        Marshal.GetLastWin32Error() + "."
                    );
                }
            }

            return sent;
        }
        finally
        {
            if (injectedShiftHeld)
            {
                Input[] releaseShift = new[] { KeyInput(VirtualKeyShift, KeyEventKeyUp) };
                SendInput(1, releaseShift, Marshal.SizeOf(typeof(Input)));
            }
            if (attachedTarget)
            {
                AttachThreadInput(currentThread, targetThread, false);
            }
            if (attachedForeground)
            {
                AttachThreadInput(currentThread, foregroundThread, false);
            }
        }
    }

    public static long GetVisibleCursorHandle()
    {
        CursorInfo cursorInfo = new CursorInfo
        {
            Size = Marshal.SizeOf(typeof(CursorInfo))
        };
        if (!GetCursorInfo(ref cursorInfo))
        {
            throw new InvalidOperationException(
                "GetCursorInfo failed with Win32 error " +
                Marshal.GetLastWin32Error() + "."
            );
        }

        const int CursorShowing = 0x00000001;
        if ((cursorInfo.Flags & CursorShowing) == 0 || cursorInfo.Cursor == IntPtr.Zero)
        {
            return 0;
        }
        return cursorInfo.Cursor.ToInt64();
    }

    public static long SetIBeamCursor()
    {
        const int IBeamCursorId = 32513;
        IntPtr cursor = LoadCursor(IntPtr.Zero, new IntPtr(IBeamCursorId));
        if (cursor == IntPtr.Zero)
        {
            throw new InvalidOperationException(
                "Could not load the system I-beam cursor. Win32 error " +
                Marshal.GetLastWin32Error() + "."
            );
        }
        SetCursor(cursor);
        return cursor.ToInt64();
    }

    public static IntPtr CreateFocusProbeWindow(int x, int y, int width, int height)
    {
        const uint ExtendedWindowStyle = 0x00000008;
        const uint WindowStyle = 0x00CF0000;
        const int ShowNormally = 1;
        IntPtr window = CreateWindowEx(
            ExtendedWindowStyle,
            "BUTTON",
            "Echo cursor focus probe",
            WindowStyle,
            x,
            y,
            width,
            height,
            IntPtr.Zero,
            IntPtr.Zero,
            IntPtr.Zero,
            IntPtr.Zero
        );
        if (window == IntPtr.Zero)
        {
            throw new InvalidOperationException(
                "Could not create the cursor focus probe window. Win32 error " +
                Marshal.GetLastWin32Error() + "."
            );
        }
        ShowWindow(window, ShowNormally);
        return window;
    }

    public static bool FocusProbeWindow(IntPtr window)
    {
        const uint NoSize = 0x0001;
        const uint NoMove = 0x0002;
        const uint ShowWindowFlag = 0x0040;
        uint currentThread = GetCurrentThreadId();
        uint ignoredProcess;
        IntPtr previousForeground = GetForegroundWindow();
        uint foregroundThread = previousForeground == IntPtr.Zero
            ? 0
            : GetWindowThreadProcessId(previousForeground, out ignoredProcess);
        bool attachedForeground = false;
        try
        {
            if (foregroundThread != 0 && foregroundThread != currentThread)
            {
                attachedForeground = AttachThreadInput(currentThread, foregroundThread, true);
                if (!attachedForeground)
                {
                    return false;
                }
            }

            if (!SetWindowPos(
                window,
                new IntPtr(-1),
                0,
                0,
                0,
                0,
                NoSize | NoMove | ShowWindowFlag
            ))
            {
                return false;
            }
            BringWindowToTop(window);
            SetForegroundWindow(window);
            SetFocus(window);
            return GetForegroundWindow() == window;
        }
        finally
        {
            if (attachedForeground)
            {
                AttachThreadInput(currentThread, foregroundThread, false);
            }
        }
    }
}
"@

function Save-WindowScreenshot {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,

        [Parameter(Mandatory = $true)]
        [IntPtr]$WindowHandle,

        [switch]$VisibleSurface
    )

    Add-Type -AssemblyName System.Drawing

    $rect = [EchoLiveUiNativeMethods+Rect]::new()
    if (-not [EchoLiveUiNativeMethods]::GetWindowRect($WindowHandle, [ref]$rect)) {
        throw "Could not read Echo's main-window bounds."
    }
    $width = $rect.Right - $rect.Left
    $height = $rect.Bottom - $rect.Top
    if ($width -le 0 -or $height -le 0) {
        throw "Echo's main-window bounds are invalid: ${width}x${height}."
    }

    $bitmap = [System.Drawing.Bitmap]::new($width, $height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        if ($VisibleSurface) {
            # GPU-composited input overlays can lag PrintWindow's off-screen
            # backing surface by a frame even though the visible foreground
            # window is current. Completion checkpoints are captured from the
            # launch-pinned foreground rectangle after the popup is closed.
            $graphics.CopyFromScreen(
                $rect.Left,
                $rect.Top,
                0,
                0,
                [System.Drawing.Size]::new($width, $height),
                [System.Drawing.CopyPixelOperation]::SourceCopy
            )
        } else {
            $deviceContext = $graphics.GetHdc()
            try {
                # PW_RENDERFULLCONTENT (2) captures the requested HWND instead
                # of whatever window happens to cover its screen bounds.
                if (-not [EchoLiveUiNativeMethods]::PrintWindow($WindowHandle, $deviceContext, 2)) {
                    throw "Could not capture Echo's main window through PrintWindow."
                }
            } finally {
                $graphics.ReleaseHdc($deviceContext)
            }
        }
        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

function Send-NativeTabKey {
    param(
        [Parameter(Mandatory = $true)]
        [IntPtr]$WindowHandle,

        [switch]$Reverse
    )

    # The completion fixture prepares the draft and GPUI focus in-process. This
    # step attaches to Echo's native input queue, verifies its HWND owns focus,
    # then uses SendInput for the real Windows -> GPUI Tab/Shift+Tab boundary.
    # No text, Enter, send action, or moderation input is injected.
    $expectedInputCount = if ($Reverse) { 4 } else { 2 }
    $sentInputCount = [EchoLiveUiNativeMethods]::SendTabToWindow($WindowHandle, $Reverse.IsPresent)
    if ($sentInputCount -ne $expectedInputCount) {
        throw "Native Tab injection reported $sentInputCount of $expectedInputCount expected events."
    }
    Start-Sleep -Milliseconds 100
}

function Invoke-UiFixtureKnownBidiSelection {
    param(
        [Parameter(Mandatory = $true)]
        [IntPtr]$WindowHandle,

        [Parameter(Mandatory = $true)]
        [string]$MarkerLine,

        [Parameter(Mandatory = $true)]
        [string]$TargetName
    )

    # The renderer derives both interior endpoints with the same shaped-line
    # closest-index hit test used by production pointer selection. Validate
    # them before injecting one native mouse down, bounded physical movement,
    # and one native mouse up. This never types, invokes Enter, or sends.
    $x = Get-UiFixtureMarkerNumber -Line $MarkerLine -Field "x"
    $y = Get-UiFixtureMarkerNumber -Line $MarkerLine -Field "y"
    $width = Get-UiFixtureMarkerNumber -Line $MarkerLine -Field "width"
    $height = Get-UiFixtureMarkerNumber -Line $MarkerLine -Field "height"
    $targetSelectionStart = [int](Get-UiFixtureMarkerNumber `
        -Line $MarkerLine `
        -Field "target_selection_start")
    $targetSelectionEnd = [int](Get-UiFixtureMarkerNumber `
        -Line $MarkerLine `
        -Field "target_selection_end")
    $selectionStartLocalX = Get-UiFixtureMarkerNumber `
        -Line $MarkerLine `
        -Field "selection_start_local_x"
    $selectionEndLocalX = Get-UiFixtureMarkerNumber `
        -Line $MarkerLine `
        -Field "selection_end_local_x"
    if (
        $width -le 0 -or
        $height -le 0 -or
        $targetSelectionStart -lt 0 -or
        $targetSelectionEnd -le $targetSelectionStart -or
        $selectionStartLocalX -le 0 -or
        $selectionStartLocalX -ge $width -or
        $selectionEndLocalX -le 0 -or
        $selectionEndLocalX -ge $width -or
        $selectionStartLocalX -eq $selectionEndLocalX
    ) {
        throw "UI fixture target '$TargetName' exposed invalid bidi range $targetSelectionStart..$targetSelectionEnd or endpoints start=$selectionStartLocalX end=$selectionEndLocalX bounds=${width}x${height}."
    }

    $startPoint = Move-CursorToUiFixtureTarget `
        -WindowHandle $WindowHandle `
        -MarkerLine $MarkerLine `
        -TargetName "$TargetName logical boundary $targetSelectionStart" `
        -XFraction ($selectionStartLocalX / $width)
    $endPoint = Move-CursorToUiFixtureTarget `
        -WindowHandle $WindowHandle `
        -MarkerLine $MarkerLine `
        -TargetName "$TargetName logical boundary $targetSelectionEnd" `
        -XFraction ($selectionEndLocalX / $width)

    $startClientX = [int][Math]::Round($x + $selectionStartLocalX)
    $endClientX = [int][Math]::Round($x + $selectionEndLocalX)
    $clientY = [int][Math]::Round($y + ($height * 0.5))
    $targetRoot = [EchoLiveUiNativeMethods]::GetAncestor($WindowHandle, 2)
    if ($targetRoot -eq [IntPtr]::Zero) {
        $targetRoot = $WindowHandle
    }
    if (-not [EchoLiveUiNativeMethods]::SetCursorPos($startPoint.X, $startPoint.Y)) {
        throw "Could not position the pointer at '$TargetName' logical boundary $targetSelectionStart."
    }
    Start-Sleep -Milliseconds 50
    [EchoLiveUiNativeMethods]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    try {
        Start-Sleep -Milliseconds 100
        $foregroundRoot = [EchoLiveUiNativeMethods]::GetForegroundWindow()
        if ($foregroundRoot -ne [IntPtr]::Zero) {
            $resolvedForegroundRoot = [EchoLiveUiNativeMethods]::GetAncestor($foregroundRoot, 2)
            if ($resolvedForegroundRoot -ne [IntPtr]::Zero) {
                $foregroundRoot = $resolvedForegroundRoot
            }
        }
        if ($foregroundRoot -ne $targetRoot) {
            throw "Echo did not own native foreground after mixed-direction pointer down (foreground=$foregroundRoot target=$targetRoot)."
        }
        foreach ($step in 1..8) {
            $progress = $step / 8.0
            $stepScreenX = [int][Math]::Round(
                $startPoint.X + (($endPoint.X - $startPoint.X) * $progress)
            )
            $stepClientX = [int][Math]::Round(
                $startClientX + (($endClientX - $startClientX) * $progress)
            )
            if (-not [EchoLiveUiNativeMethods]::SetCursorPos($stepScreenX, $startPoint.Y)) {
                throw "Could not move the pressed pointer across '$TargetName' at step $step."
            }
            $mouseMoveLParam = [IntPtr]((($clientY -band 0xffff) -shl 16) -bor ($stepClientX -band 0xffff))
            if (-not [EchoLiveUiNativeMethods]::PostMessage(
                $WindowHandle,
                0x0200,
                [IntPtr]1,
                $mouseMoveLParam
            )) {
                throw "Could not publish pressed pointer motion for '$TargetName' at step $step."
            }
            Start-Sleep -Milliseconds 25
        }
    } finally {
        [EchoLiveUiNativeMethods]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    }
}

function Assert-NativeCursorStable {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Context,

        [ValidateRange(2, 240)]
        [int]$SampleCount = 12,

        [long]$ExpectedHandle = 0
    )

    # A stationary pointer must remain both visible and on one native cursor
    # handle while animated Echo frames continue to present. Sampling after
    # the renderer-owned completion result marker avoids treating an intended
    # surface transition as cursor flicker.
    $previousHandle = [long]0
    $handleChanges = 0
    foreach ($sample in 1..$SampleCount) {
        $cursorHandle = [EchoLiveUiNativeMethods]::GetVisibleCursorHandle()
        if ($cursorHandle -eq 0) {
            throw "The native cursor became hidden during '$Context' (sample $sample of $SampleCount)."
        }
        if ($ExpectedHandle -ne 0 -and $cursorHandle -ne $ExpectedHandle) {
            throw "The native cursor changed ownership during '$Context' (sample $sample of $SampleCount, expected handle $ExpectedHandle, observed $cursorHandle)."
        }
        if ($previousHandle -ne 0 -and $cursorHandle -ne $previousHandle) {
            $handleChanges += 1
        }
        $previousHandle = $cursorHandle
        Start-Sleep -Milliseconds 25
    }

    if ($handleChanges -ne 0) {
        throw "The native cursor changed handles $handleChanges time(s) during '$Context' while the pointer was stationary."
    }
    $result = "cursor_stability context=$Context visible_samples=$SampleCount handle_changes=0 passed=true"
    if (-not [string]::IsNullOrWhiteSpace($script:cursorStabilityPath)) {
        Add-Content -LiteralPath $script:cursorStabilityPath -Value $result
    }
    Write-Output $result
}

function Set-NativeCursorTargetOverWindow {
    param(
        [Parameter(Mandatory = $true)]
        [IntPtr]$WindowHandle
    )

    $windowRect = [EchoLiveUiNativeMethods+Rect]::new()
    if (-not [EchoLiveUiNativeMethods]::GetWindowRect($WindowHandle, [ref]$windowRect)) {
        throw "Could not read Echo's main-window bounds for the cursor baseline."
    }
    $cursorPoint = [EchoLiveUiNativeMethods+Point]::new()
    $cursorPoint.X = [int](($windowRect.Left + $windowRect.Right) / 2)
    $cursorPoint.Y = [int](($windowRect.Top + $windowRect.Bottom) / 2)

    # Moving to an adjacent point first guarantees Windows emits real pointer
    # motion even if the previous run left the cursor at this exact target.
    [void][EchoLiveUiNativeMethods]::SetCursorPos($cursorPoint.X - 1, $cursorPoint.Y)
    if (-not [EchoLiveUiNativeMethods]::SetCursorPos($cursorPoint.X, $cursorPoint.Y)) {
        throw "Could not position the pointer over Echo for the cursor baseline."
    }
    Start-Sleep -Milliseconds 100

    $windowAtCursor = [EchoLiveUiNativeMethods]::WindowFromPoint($cursorPoint)
    $rootAtCursor = [EchoLiveUiNativeMethods]::GetAncestor($windowAtCursor, 2)
    if ($rootAtCursor -ne $WindowHandle) {
        throw "The cursor baseline target is not owned by Echo's launch-pinned main window."
    }
    Assert-NativeCursorStable -Context "visible baseline before native keys"
}

function Assert-NativeCursorStableWhileEchoUnfocused {
    param(
        [Parameter(Mandatory = $true)]
        [IntPtr]$MainWindowHandle
    )

    $mainRect = [EchoLiveUiNativeMethods+Rect]::new()
    if (-not [EchoLiveUiNativeMethods]::GetWindowRect($MainWindowHandle, [ref]$mainRect)) {
        throw "Could not read Echo's main-window bounds for the unfocused cursor checkpoint."
    }
    $cursorPoint = [EchoLiveUiNativeMethods+Point]::new()
    $cursorPoint.X = $mainRect.Right - 48
    $cursorPoint.Y = $mainRect.Bottom - 48
    if (-not [EchoLiveUiNativeMethods]::SetCursorPos($cursorPoint.X, $cursorPoint.Y)) {
        throw "Could not position the pointer over Echo for the unfocused cursor checkpoint."
    }

    $windowAtCursor = [EchoLiveUiNativeMethods]::WindowFromPoint($cursorPoint)
    $rootAtCursor = [EchoLiveUiNativeMethods]::GetAncestor($windowAtCursor, 2)
    if ($rootAtCursor -ne $MainWindowHandle) {
        throw "The unfocused cursor checkpoint was occluded before focus transfer."
    }

    $probeWindow = [IntPtr]::Zero
    $coveringProbeWindow = [IntPtr]::Zero
    try {
        $probeWindow = [EchoLiveUiNativeMethods]::CreateFocusProbeWindow(
            $mainRect.Left,
            $mainRect.Top,
            280,
            120
        )
        if (-not [EchoLiveUiNativeMethods]::FocusProbeWindow($probeWindow)) {
            throw "Could not transfer native foreground focus away from Echo."
        }
        Start-Sleep -Milliseconds 250
        if ([EchoLiveUiNativeMethods]::GetForegroundWindow() -ne $probeWindow) {
            throw "Echo unexpectedly retained native foreground focus."
        }

        $windowAtCursor = [EchoLiveUiNativeMethods]::WindowFromPoint($cursorPoint)
        $rootAtCursor = [EchoLiveUiNativeMethods]::GetAncestor($windowAtCursor, 2)
        if ($rootAtCursor -ne $MainWindowHandle) {
            throw "The focus probe covered the stationary pointer instead of leaving it over Echo."
        }

        Assert-NativeCursorStable `
            -Context "unfocused Echo animated window" `
            -SampleCount 40

        [void][EchoLiveUiNativeMethods]::DestroyWindow($probeWindow)
        $probeWindow = [IntPtr]::Zero

        $coveringProbeWindow = [EchoLiveUiNativeMethods]::CreateFocusProbeWindow(
            $mainRect.Left,
            $mainRect.Top,
            ($mainRect.Right - $mainRect.Left),
            ($mainRect.Bottom - $mainRect.Top)
        )
        if (-not [EchoLiveUiNativeMethods]::FocusProbeWindow($coveringProbeWindow)) {
            throw "Could not focus the cursor-ownership covering probe."
        }
        Start-Sleep -Milliseconds 250

        $windowAtCursor = [EchoLiveUiNativeMethods]::WindowFromPoint($cursorPoint)
        $rootAtCursor = [EchoLiveUiNativeMethods]::GetAncestor($windowAtCursor, 2)
        if ($rootAtCursor -ne $coveringProbeWindow) {
            $probeRect = [EchoLiveUiNativeMethods+Rect]::new()
            [void][EchoLiveUiNativeMethods]::GetWindowRect($coveringProbeWindow, [ref]$probeRect)
            throw "The cursor-ownership probe did not cover the stationary pointer: point=$($cursorPoint.X),$($cursorPoint.Y) window=$windowAtCursor root=$rootAtCursor probe=$coveringProbeWindow probe_rect=$($probeRect.Left),$($probeRect.Top),$($probeRect.Right),$($probeRect.Bottom) main_rect=$($mainRect.Left),$($mainRect.Top),$($mainRect.Right),$($mainRect.Bottom)."
        }

        $expectedIBeamHandle = [EchoLiveUiNativeMethods]::SetIBeamCursor()
        Assert-NativeCursorStable `
            -Context "covered Echo cannot republish cursor" `
            -SampleCount 40 `
            -ExpectedHandle $expectedIBeamHandle
    } finally {
        if ($probeWindow -ne [IntPtr]::Zero) {
            [void][EchoLiveUiNativeMethods]::DestroyWindow($probeWindow)
        }
        if ($coveringProbeWindow -ne [IntPtr]::Zero) {
            [void][EchoLiveUiNativeMethods]::DestroyWindow($coveringProbeWindow)
        }
        [void][EchoLiveUiNativeMethods]::SetForegroundWindow($MainWindowHandle)
        Start-Sleep -Milliseconds 150
    }
}

function Wait-UiFixtureMarker {
    param(
        [Parameter(Mandatory = $true)]
        [string]$RuntimeLogPath,

        [Parameter(Mandatory = $true)]
        [string]$Pattern,

        [Parameter(Mandatory = $true)]
        [string]$Description,

        [Parameter(Mandatory = $true)]
        [System.Diagnostics.Process]$EchoProcess,

        [string]$MarkerNamespace = "bench\.ui_fixture",

        [ValidateRange(1, 120)]
        [int]$TimeoutSeconds = 30
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        $EchoProcess.Refresh()
        if ($EchoProcess.HasExited) {
            throw "Echo exited while waiting for UI fixture marker '$Description'."
        }

        if (Test-Path $RuntimeLogPath) {
            $uiFixtureLines = @(
                Get-Content -LiteralPath $RuntimeLogPath -Tail 1000 -ErrorAction SilentlyContinue |
                    Where-Object { $_ -match $MarkerNamespace }
            )
            $failureLine = $uiFixtureLines |
                Where-Object {
                    $_ -match "$MarkerNamespace failed(?:\s|$)" -or
                    $_ -match "$MarkerNamespace.*(?:^|\s)passed=false(?:\s|$)" -or
                    $_ -match "$MarkerNamespace .*?(?:^|\s)sent=true(?:\s|$)"
                } |
                Select-Object -First 1
            if (-not [string]::IsNullOrWhiteSpace($failureLine)) {
                throw "UI fixture automation emitted a failure marker while waiting for '$Description': $failureLine"
            }

            $matchedLine = $uiFixtureLines |
                Where-Object { $_ -match $Pattern } |
                Select-Object -Last 1
            if (-not [string]::IsNullOrWhiteSpace($matchedLine)) {
                return $matchedLine
            }
        }

        Start-Sleep -Milliseconds 50
    }

    throw "UI fixture automation did not emit '$Description' within $TimeoutSeconds seconds."
}

function Get-UiFixtureMarkerNumber {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Line,

        [Parameter(Mandatory = $true)]
        [string]$Field
    )

    $match = [regex]::Match(
        $Line,
        "(?:^|\s)$([regex]::Escape($Field))=(-?\d+(?:\.\d+)?)(?:\s|$)"
    )
    if (-not $match.Success) {
        throw "UI fixture marker is missing numeric field '$Field': $Line"
    }

    return [double]::Parse(
        $match.Groups[1].Value,
        [System.Globalization.CultureInfo]::InvariantCulture
    )
}

function Move-CursorToUiFixtureTarget {
    param(
        [Parameter(Mandatory = $true)]
        [IntPtr]$WindowHandle,

        [Parameter(Mandatory = $true)]
        [string]$MarkerLine,

        [Parameter(Mandatory = $true)]
        [string]$TargetName,

        [ValidateRange(0.0, 1.0)]
        [double]$XFraction = 0.5,

        [ValidateRange(0.0, 1.0)]
        [double]$YFraction = 0.5
    )

    $x = Get-UiFixtureMarkerNumber -Line $MarkerLine -Field "x"
    $y = Get-UiFixtureMarkerNumber -Line $MarkerLine -Field "y"
    $width = Get-UiFixtureMarkerNumber -Line $MarkerLine -Field "width"
    $height = Get-UiFixtureMarkerNumber -Line $MarkerLine -Field "height"
    if ($width -le 0 -or $height -le 0) {
        throw "UI fixture target '$TargetName' has invalid size ${width}x${height}."
    }

    $clientRect = [EchoLiveUiNativeMethods+Rect]::new()
    if (-not [EchoLiveUiNativeMethods]::GetClientRect($WindowHandle, [ref]$clientRect)) {
        throw "Could not read Echo's client bounds for UI fixture target '$TargetName'."
    }

    $targetX = [int][Math]::Round($x + ($width * $XFraction))
    $targetY = [int][Math]::Round($y + ($height * $YFraction))
    if (
        $targetX -lt $clientRect.Left -or
        $targetY -lt $clientRect.Top -or
        $targetX -ge $clientRect.Right -or
        $targetY -ge $clientRect.Bottom
    ) {
        throw "UI fixture target '$TargetName' point ($targetX,$targetY) is outside Echo's client bounds $($clientRect.Right)x$($clientRect.Bottom)."
    }

    $screenPoint = [EchoLiveUiNativeMethods+Point]::new()
    $screenPoint.X = $targetX
    $screenPoint.Y = $targetY
    if (-not [EchoLiveUiNativeMethods]::ClientToScreen($WindowHandle, [ref]$screenPoint)) {
        throw "Could not convert UI fixture target '$TargetName' to screen coordinates."
    }

    # SetCursorPos can leave GPUI without a fresh WM_MOUSEMOVE when Windows
    # coalesces a stationary or same-cell update. Enter from one physical pixel
    # to the left, then settle on the requested point, so hover-only contracts
    # observe a real native motion edge without clicking or sending input.
    if (-not [EchoLiveUiNativeMethods]::SetCursorPos($screenPoint.X - 1, $screenPoint.Y)) {
        throw "Could not stage the pointer beside UI fixture target '$TargetName'."
    }
    Start-Sleep -Milliseconds 20
    if (-not [EchoLiveUiNativeMethods]::SetCursorPos($screenPoint.X, $screenPoint.Y)) {
        throw "Could not move the pointer to UI fixture target '$TargetName'."
    }

    Start-Sleep -Milliseconds 100
    $cursorInfo = [EchoLiveUiNativeMethods+CursorInfo]::new()
    $cursorInfo.Size = [Runtime.InteropServices.Marshal]::SizeOf([type][EchoLiveUiNativeMethods+CursorInfo])
    if (-not [EchoLiveUiNativeMethods]::GetCursorInfo([ref]$cursorInfo)) {
        throw "Could not verify the native pointer position for UI fixture target '$TargetName'."
    }
    $windowAtPoint = [EchoLiveUiNativeMethods]::WindowFromPoint($screenPoint)
    # WindowFromPoint may return Echo's render/input child HWND. Accept only
    # when its GA_ROOT owner matches the root of the launch-pinned main client.
    $expectedRootWindow = [EchoLiveUiNativeMethods]::GetAncestor($WindowHandle, 2)
    if ($expectedRootWindow -eq [IntPtr]::Zero) {
        $expectedRootWindow = $WindowHandle
    }
    $rootWindowAtPoint = [EchoLiveUiNativeMethods]::GetAncestor($windowAtPoint, 2)
    if ($windowAtPoint -ne $WindowHandle -and $rootWindowAtPoint -ne $expectedRootWindow) {
        $occluderProcessId = [uint32]0
        [void][EchoLiveUiNativeMethods]::GetWindowThreadProcessId(
            $rootWindowAtPoint,
            [ref]$occluderProcessId
        )
        $occluderTitleLength = [EchoLiveUiNativeMethods]::GetWindowTextLength($rootWindowAtPoint)
        $occluderTitle = [System.Text.StringBuilder]::new([Math]::Max(1, $occluderTitleLength + 1))
        [void][EchoLiveUiNativeMethods]::GetWindowText(
            $rootWindowAtPoint,
            $occluderTitle,
            $occluderTitle.Capacity
        )
        throw "UI fixture target '$TargetName' is occluded by window handle $windowAtPoint (root $rootWindowAtPoint, process $occluderProcessId, title '$($occluderTitle.ToString())', expected root $expectedRootWindow); refusing to inject input."
    }

    # GPUI consumes WM_MOUSEMOVE from the launch-pinned client. Windows can
    # coalesce SetCursorPos movement before that client observes it, especially
    # after focus-probe checkpoints. Publish the verified client coordinates to
    # the same native HWND after the occlusion/ownership gate; this carries no
    # button state and cannot click, type, send, or target another process.
    $mouseMoveLParam = [IntPtr]((($targetY -band 0xffff) -shl 16) -bor ($targetX -band 0xffff))
    if (-not [EchoLiveUiNativeMethods]::PostMessage(
        $WindowHandle,
        0x0200,
        [IntPtr]::Zero,
        $mouseMoveLParam
    )) {
        throw "Could not publish native pointer motion to UI fixture target '$TargetName'."
    }
    Start-Sleep -Milliseconds 50

    Write-Host (
        "UI fixture pointer target='$TargetName' " +
        "client=($targetX,$targetY) screen=($($screenPoint.X),$($screenPoint.Y)) " +
        "actual_screen=($($cursorInfo.ScreenPosition.X),$($cursorInfo.ScreenPosition.Y)) " +
        "window=$windowAtPoint root=$rootWindowAtPoint expected_root=$expectedRootWindow"
    )

    return $screenPoint
}

function Invoke-UiFixtureSafeLeftClick {
    param(
        [Parameter(Mandatory = $true)]
        [IntPtr]$WindowHandle,

        [Parameter(Mandatory = $true)]
        [string]$MarkerLine,

        [Parameter(Mandatory = $true)]
        [string]$TargetName
    )

    $screenPoint = Move-CursorToUiFixtureTarget `
        -WindowHandle $WindowHandle `
        -MarkerLine $MarkerLine `
        -TargetName $TargetName
    [void][EchoLiveUiNativeMethods]::SetForegroundWindow($WindowHandle)
    Start-Sleep -Milliseconds 75

    # Use the same real Windows mouse boundary that exercises composer click
    # counting and drag selection. The cursor target, foreground client, and
    # HWND ownership were already verified above, and this helper remains
    # restricted to fixture-safe controls that cannot send chat or moderate.
    [EchoLiveUiNativeMethods]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 150
    # GPUI's slider owns a drag recognizer, so a stationary press can be
    # consumed before its click handlers run. Cross the recognizer threshold
    # by a bounded eight pixels while remaining well inside the 140 px target.
    [void][EchoLiveUiNativeMethods]::SetCursorPos($screenPoint.X + 8, $screenPoint.Y)
    Start-Sleep -Milliseconds 150
    [EchoLiveUiNativeMethods]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
}

function Invoke-UiFixtureSafeTripleClick {
    param(
        [Parameter(Mandatory = $true)]
        [IntPtr]$WindowHandle,

        [Parameter(Mandatory = $true)]
        [string]$MarkerLine,

        [Parameter(Mandatory = $true)]
        [string]$TargetName
    )

    [void](Move-CursorToUiFixtureTarget `
        -WindowHandle $WindowHandle `
        -MarkerLine $MarkerLine `
        -TargetName $TargetName)
    [void][EchoLiveUiNativeMethods]::SetForegroundWindow($WindowHandle)
    Start-Sleep -Milliseconds 75

    # Three bounded left clicks exercise GPUI's real click-count path. This is
    # restricted to the fixture-owned composer draft and never types text or
    # invokes Enter/Send.
    foreach ($click in 1..3) {
        [EchoLiveUiNativeMethods]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 35
        [EchoLiveUiNativeMethods]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
        if ($click -lt 3) {
            Start-Sleep -Milliseconds 60
        }
    }
}

function Invoke-UiFixtureSafeReverseDrag {
    param(
        [Parameter(Mandatory = $true)]
        [IntPtr]$WindowHandle,

        [Parameter(Mandatory = $true)]
        [string]$MarkerLine,

        [Parameter(Mandatory = $true)]
        [string]$TargetName
    )

    # Validate both endpoints through the same Echo-root ownership and client
    # bounds gate as every other native fixture interaction. These first-line
    # fractions keep the moving caret clear of the action rail and finish it on
    # the left, exercising reversed selection state.
    $startPoint = Move-CursorToUiFixtureTarget `
        -WindowHandle $WindowHandle `
        -MarkerLine $MarkerLine `
        -TargetName "$TargetName start" `
        -XFraction 0.78 `
        -YFraction 0.22
    $endPoint = Move-CursorToUiFixtureTarget `
        -WindowHandle $WindowHandle `
        -MarkerLine $MarkerLine `
        -TargetName "$TargetName end" `
        -XFraction 0.12 `
        -YFraction 0.22

    [void][EchoLiveUiNativeMethods]::SetForegroundWindow($WindowHandle)
    [void][EchoLiveUiNativeMethods]::SetCursorPos($startPoint.X, $startPoint.Y)
    Start-Sleep -Milliseconds 75
    [EchoLiveUiNativeMethods]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    try {
        foreach ($step in 1..8) {
            $progress = $step / 8.0
            $stepX = [int][Math]::Round(
                $startPoint.X + (($endPoint.X - $startPoint.X) * $progress)
            )
            $stepY = [int][Math]::Round(
                $startPoint.Y + (($endPoint.Y - $startPoint.Y) * $progress)
            )
            [void][EchoLiveUiNativeMethods]::SetCursorPos($stepX, $stepY)
            Start-Sleep -Milliseconds 20
        }
    } finally {
        [EchoLiveUiNativeMethods]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    }
}

function Set-UiFixtureWindowSize {
    param(
        [Parameter(Mandatory = $true)]
        [IntPtr]$WindowHandle,

        [Parameter(Mandatory = $true)]
        [string]$TargetName,

        [ValidateRange(320, 4096)]
        [int]$Width,

        [ValidateRange(320, 4096)]
        [int]$Height,

        [ValidateRange(1, 4096)]
        [int]$MinimumObservedWidth,

        [ValidateRange(1, 4096)]
        [int]$MaximumObservedWidth
    )

    $originalRect = [EchoLiveUiNativeMethods+Rect]::new()
    if (-not [EchoLiveUiNativeMethods]::GetWindowRect($WindowHandle, [ref]$originalRect)) {
        throw "Could not read bounds for UI fixture window '$TargetName'."
    }

    # SWP_NOZORDER | SWP_NOACTIVATE. Keep the popup at its current position and
    # never raise/focus an unrelated window while changing only its dimensions.
    if (-not [EchoLiveUiNativeMethods]::SetWindowPos(
        $WindowHandle,
        [IntPtr]::Zero,
        $originalRect.Left,
        $originalRect.Top,
        $Width,
        $Height,
        0x0014
    )) {
        throw "Could not resize UI fixture window '$TargetName' to ${Width}x${Height}."
    }

    Start-Sleep -Milliseconds 200
    $observedRect = [EchoLiveUiNativeMethods+Rect]::new()
    if (-not [EchoLiveUiNativeMethods]::GetWindowRect($WindowHandle, [ref]$observedRect)) {
        throw "Could not verify resized bounds for UI fixture window '$TargetName'."
    }
    $observedWidth = $observedRect.Right - $observedRect.Left
    $observedHeight = $observedRect.Bottom - $observedRect.Top
    if (
        $observedWidth -lt $MinimumObservedWidth -or
        $observedWidth -gt $MaximumObservedWidth -or
        $observedHeight -lt 680
    ) {
        throw "UI fixture window '$TargetName' resized to unexpected bounds ${observedWidth}x${observedHeight}."
    }

    return $observedRect
}

function Restore-UiFixtureWindowBounds {
    param(
        [Parameter(Mandatory = $true)]
        [IntPtr]$WindowHandle,

        [Parameter(Mandatory = $true)]
        [object]$Bounds,

        [Parameter(Mandatory = $true)]
        [string]$TargetName
    )

    $width = $Bounds.Right - $Bounds.Left
    $height = $Bounds.Bottom - $Bounds.Top
    if ($width -le 0 -or $height -le 0) {
        throw "Cannot restore UI fixture window '$TargetName' to invalid bounds ${width}x${height}."
    }

    # SWP_NOZORDER | SWP_NOACTIVATE. Restoration changes only the launch-pinned
    # Echo window and never raises, focuses, or resizes another process.
    if (-not [EchoLiveUiNativeMethods]::SetWindowPos(
        $WindowHandle,
        [IntPtr]::Zero,
        $Bounds.Left,
        $Bounds.Top,
        $width,
        $height,
        0x0014
    )) {
        throw "Could not restore UI fixture window '$TargetName' to ${width}x${height}."
    }

    Start-Sleep -Milliseconds 200
    $observed = [EchoLiveUiNativeMethods+Rect]::new()
    if (-not [EchoLiveUiNativeMethods]::GetWindowRect($WindowHandle, [ref]$observed)) {
        throw "Could not verify restored bounds for UI fixture window '$TargetName'."
    }
    $observedWidth = $observed.Right - $observed.Left
    $observedHeight = $observed.Bottom - $observed.Top
    if ([Math]::Abs($observedWidth - $width) -gt 16 -or [Math]::Abs($observedHeight - $height) -gt 16) {
        throw "UI fixture window '$TargetName' restored to unexpected bounds ${observedWidth}x${observedHeight}; expected ${width}x${height}."
    }
}

function Find-ProcessWindowByTitlePrefix {
    param(
        [Parameter(Mandatory = $true)]
        [int]$ProcessId,

        [Parameter(Mandatory = $true)]
        [string]$TitlePrefix
    )

    $matchedWindow = [IntPtr]::Zero
    $callback = [EchoLiveUiNativeMethods+EnumWindowsProc] {
        param([IntPtr]$window, [IntPtr]$parameter)

        if (-not [EchoLiveUiNativeMethods]::IsWindowVisible($window)) {
            return $true
        }
        [uint32]$ownerProcessId = 0
        [void][EchoLiveUiNativeMethods]::GetWindowThreadProcessId($window, [ref]$ownerProcessId)
        if ($ownerProcessId -ne $ProcessId) {
            return $true
        }
        $titleLength = [EchoLiveUiNativeMethods]::GetWindowTextLength($window)
        if ($titleLength -le 0) {
            return $true
        }
        $title = [System.Text.StringBuilder]::new($titleLength + 1)
        [void][EchoLiveUiNativeMethods]::GetWindowText($window, $title, $title.Capacity)
        if ($title.ToString().StartsWith($TitlePrefix, [StringComparison]::OrdinalIgnoreCase)) {
            $script:matchedPickerWindow = $window
            return $false
        }
        return $true
    }

    $script:matchedPickerWindow = [IntPtr]::Zero
    [void][EchoLiveUiNativeMethods]::EnumWindows($callback, [IntPtr]::Zero)
    $matchedWindow = $script:matchedPickerWindow
    Remove-Variable -Name matchedPickerWindow -Scope Script -ErrorAction SilentlyContinue
    return $matchedWindow
}

function Wait-ProcessWindowClosedByTitlePrefix {
    param(
        [Parameter(Mandatory = $true)]
        [int]$ProcessId,

        [Parameter(Mandatory = $true)]
        [string]$TitlePrefix,

        [ValidateRange(1, 60)]
        [int]$TimeoutSeconds = 15
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        $window = Find-ProcessWindowByTitlePrefix `
            -ProcessId $ProcessId `
            -TitlePrefix $TitlePrefix
        if ($window -eq [IntPtr]::Zero) {
            return
        }
        Start-Sleep -Milliseconds 50
    }

    throw "Window '$TitlePrefix' did not close within $TimeoutSeconds seconds."
}

function Close-VisibleProcessWindows {
    param(
        [Parameter(Mandatory = $true)]
        [int]$ProcessId
    )

    $script:closedProcessWindowCount = 0
    $callback = [EchoLiveUiNativeMethods+EnumWindowsProc] {
        param([IntPtr]$window, [IntPtr]$parameter)

        if (-not [EchoLiveUiNativeMethods]::IsWindowVisible($window)) {
            return $true
        }
        [uint32]$ownerProcessId = 0
        [void][EchoLiveUiNativeMethods]::GetWindowThreadProcessId($window, [ref]$ownerProcessId)
        if ($ownerProcessId -eq $ProcessId) {
            # WM_CLOSE
            if ([EchoLiveUiNativeMethods]::PostMessage($window, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)) {
                $script:closedProcessWindowCount++
            }
        }
        return $true
    }

    [void][EchoLiveUiNativeMethods]::EnumWindows($callback, [IntPtr]::Zero)
    $closedCount = $script:closedProcessWindowCount
    Remove-Variable -Name closedProcessWindowCount -Scope Script -ErrorAction SilentlyContinue
    return $closedCount
}

$scriptRoot = Split-Path -Parent $PSCommandPath
$repoRoot = (Resolve-Path (Join-Path $scriptRoot "..\..")).Path
$resolvedLaunchPath = Resolve-ExecutablePath -Path (Join-Path $repoRoot $LaunchPath)
$resolvedCheckerPath = Resolve-ExecutablePath -Path (Join-Path $repoRoot $ProfileLogCheckPath)
$pickerChannelWasExplicit = $PSBoundParameters.ContainsKey("EmotePickerSweepChannel")
$effectivePickerSweepChannel = if (($UiFixtureInteractions -or $LinkPreviewMatrix) -and -not $pickerChannelWasExplicit) {
    ""
} else {
    $EmotePickerSweepChannel
}
$pickerSweepEnabled = -not [string]::IsNullOrWhiteSpace($effectivePickerSweepChannel)
$pickerSweepChannel = $effectivePickerSweepChannel.Trim().TrimStart('#').ToLowerInvariant()
if ($UiFixtureSettingsOnly -and -not $UiFixtureInteractions) {
    throw "-UiFixtureSettingsOnly requires -UiFixtureInteractions."
}
if ($FeedbackUiCheckpoints -and -not $UiFixtureInteractions) {
    throw "-FeedbackUiCheckpoints requires -UiFixtureInteractions."
}
if ($UiFixtureInteractions -and $pickerChannelWasExplicit -and $pickerSweepEnabled) {
    throw "-UiFixtureInteractions cannot be combined with -EmotePickerSweepChannel. Run the disposable UI fixture and the saved-layout picker sweep as separate GUI automation sessions."
}
if ($LinkPreviewMatrix) {
    if ($UiFixtureInteractions -or $UiFixtureSettingsOnly -or $FeedbackUiCheckpoints -or $CursorStabilityCheckpoints -or $PopupMonitorCheckpoints -or $pickerSweepEnabled) {
        throw "-LinkPreviewMatrix is a dedicated isolated live no-send acceptance path and cannot be combined with UI fixture, cursor, popup, or picker checkpoints."
    }
    $LinkPreviewMatrixChannel = $LinkPreviewMatrixChannel.Trim().TrimStart('#').ToLowerInvariant()
    if ($LinkPreviewMatrixChannel -notmatch '^[a-z0-9_]{4,25}$') {
        throw "LinkPreviewMatrixChannel must be a 4-25 character ASCII Twitch login (optionally prefixed with #)."
    }
}

$existingProcesses = @(Get-Process -Name ([System.IO.Path]::GetFileNameWithoutExtension($resolvedLaunchPath)) -ErrorAction SilentlyContinue)
if ($existingProcesses.Count -gt 0) {
    throw "Echo is already running. Close the existing process before starting an automated UI suite."
}

$initializer = Join-Path $scriptRoot "new-benchmark-run.ps1"
$initializerArgs = @{
    Scenario = $Scenario
    EchoPath = $resolvedLaunchPath
    EchoBuildType = "release"
    CacheState = "warm"
    NetworkState = "live"
    FixtureName = "saved-live-layout"
    Notes = if ($LinkPreviewMatrix) {
        "Isolated live no-send link-preview matrix: connects through the ordinary #$LinkPreviewMatrixChannel lifecycle while one locally replayed deterministic direct-image link exercises display None, Manual, and Always with thumbnails None, Small, Medium, and Large. The fixture seeds terminal local metadata only; no resolver request, typed text, Enter, chat send, moderation, browser, or normal-profile mutation is permitted."
    } elseif ($CursorStabilityCheckpoints) {
        "Disposable native cursor-stability acceptance: the harness establishes a visible stationary pointer over Echo, verifies the cursor remains visible on one Win32 handle after every main/thread emote/mention Tab and Shift+Tab result, transfers foreground focus to a dedicated native probe while leaving the pointer over animated Echo, then covers that stationary point with a second probe and requires its distinct I-beam cursor to remain authoritative. No text, Enter, chat send, moderation action, destructive input, or persistent settings mutation is permitted."
    } elseif ($FeedbackUiCheckpoints) {
        "Disposable feedback UI acceptance: renderer-owned checkpoints prove channel-chooser autofocus and placeholder stability, focused settings search, an in-process multiline composer draft, narrow main-window layout, and sound-slider hover/click/restoration. Native input is limited to a bounded sound-slider hover/click plus completion Tab/Shift+Tab when completion acceptance is enabled. No text, Enter, chat send, moderation action, destructive input, or persistent settings mutation is permitted."
    } elseif ($UiFixtureSettingsOnly) {
        "Disposable completion plus settings acceptance: completion drafts/focus are prepared in-process, then real Windows Tab/Shift+Tab events cross GPUI's keybinding boundary for main/thread emote and mention acceptance/cycling; settings uses production modal controllers for default disclosure, focused search, routing, advanced reveal, empty-query safety, and exact persisted AppSettings comparison. No text, Enter, chat send, moderation action, or destructive input is injected. The app exits before unrelated legacy tab-hover automation."
    } elseif ($UiFixtureInteractions -and $pickerSweepEnabled) {
        "Scripted channel-header, wide/narrow native channel-popup, tab-strip, whisper-transport, bounded completion, and combined Activity interactions plus a maximized full-catalog emote-picker down/up sweep for $pickerSweepChannel; completion injects only native Tab/Shift+Tab after in-process draft/focus setup, while other native input remains limited to the safe tab-overflow click. No text, Enter, send action, moderation action, or user-history destructive control is used."
    } elseif ($UiFixtureInteractions) {
        "Scripted channel-header, wide/narrow native channel-popup, tab-strip, whisper-transport, bounded completion, and combined Activity interactions with named render checkpoints; completion injects only native Tab/Shift+Tab after in-process draft/focus setup, while other native input remains limited to the safe tab-overflow click. No emote-picker sweep, text, Enter, send action, moderation action, or user-history destructive control is used."
    } elseif ($pickerSweepEnabled) {
        "Scripted live tabs, split focus, scrolling, settings, quick switcher, metrics overlay, and a maximized full-catalog emote-picker down/up sweep for $pickerSweepChannel; no text input, Enter key, or send action."
    } else {
        "Scripted live tabs, split focus, scrolling, picker, settings, quick switcher, and metrics overlay; no text input, Enter key, or send action."
    }
}
if (-not [string]::IsNullOrWhiteSpace($OutputRoot)) {
    $initializerArgs.OutputRoot = $OutputRoot
}
$runDir = ((& $initializer @initializerArgs | Select-Object -Last 1) | Out-String).Trim()
if ([string]::IsNullOrWhiteSpace($runDir) -or -not (Test-Path $runDir)) {
    throw "Benchmark initializer did not return an existing run directory."
}
$runDir = (Resolve-Path $runDir).Path

$runtimeLogPath = Join-Path $runDir "$MetricName.log"
$rawFileName = "$MetricName.raw.txt"
$rawPath = Join-Path $runDir $rawFileName
$automationPath = Join-Path $runDir "ui-automation-events.raw.txt"
$uiFixturePath = Join-Path $runDir "ui-fixture-events.raw.txt"
$popupMonitorPath = Join-Path $runDir "popup-monitor-events.raw.txt"
$script:cursorStabilityPath = Join-Path $runDir "cursor-stability-events.raw.txt"
$pickerSweepPath = Join-Path $runDir "picker-sweep-events.raw.txt"
$pickerStabilityPath = Join-Path $runDir "picker-stability-events.raw.txt"
$screenshotPath = Join-Path $runDir "metrics-overlay.png"
$pickerScreenshotPath = Join-Path $runDir "emote-picker-sweep.png"
$profileCheckPath = Join-Path $runDir "profile-log-check.raw.txt"
$gpuSamplesPath = Join-Path $runDir "gpu-timestamp-samples.raw.txt"
$providerCachePath = Join-Path $runDir "provider-cache-stages.raw.txt"
$slowWarningsPath = Join-Path $runDir "slow-provider-stages.raw.txt"
$connectionEvidencePath = Join-Path $runDir "live-connection-evidence.raw.txt"
$uiFixtureHeaderTabsScreenshotPath = Join-Path $runDir "header-tabs.png"
$uiFixtureTabHoverScreenshotPath = Join-Path $runDir "tab-close-hover.png"
$uiFixtureTabDropdownScreenshotPath = Join-Path $runDir "tab-dropdown-open.png"
$uiFixtureTabMultilineScreenshotPath = Join-Path $runDir "tab-multiline.png"
$uiFixtureActivityAllScreenshotPath = Join-Path $runDir "activity-all.png"
$uiFixtureActivityMentionsScreenshotPath = Join-Path $runDir "activity-mentions.png"
$uiFixtureActivityWhispersScreenshotPath = Join-Path $runDir "activity-whispers.png"
$uiFixtureActivityEventsScreenshotPath = Join-Path $runDir "activity-events.png"
$uiFixtureChannelPopupWideScreenshotPath = Join-Path $runDir "channel-popup-wide.png"
$uiFixtureChannelPopupNarrowScreenshotPath = Join-Path $runDir "channel-popup-narrow.png"
$uiFixtureCompletionMainEmoteScreenshotPath = Join-Path $runDir "completion-main-emote-suggestions.png"
$uiFixtureCompletionMainMentionScreenshotPath = Join-Path $runDir "completion-main-mention-suggestions.png"
$uiFixtureCompletionMainCycleScreenshotPath = Join-Path $runDir "completion-main-inline-cycle.png"
$uiFixtureCompletionWatchingEmoteScreenshotPath = Join-Path $runDir "completion-watching-emote-suggestions.png"
$uiFixtureCompletionWatchingCycleScreenshotPath = Join-Path $runDir "completion-watching-inline-cycle.png"
$uiFixtureCompletionThreadEmoteScreenshotPath = Join-Path $runDir "completion-thread-emote-suggestions.png"
$uiFixtureCompletionThreadMentionScreenshotPath = Join-Path $runDir "completion-thread-mention-suggestions.png"
$uiFixtureCompletionThreadCycleScreenshotPath = Join-Path $runDir "completion-thread-inline-cycle.png"
$uiFixtureSettingsDefaultScreenshotPath = Join-Path $runDir "settings-default.png"
$uiFixtureSettingsUpdatesUpToDateScreenshotPath = Join-Path $runDir "settings-updates-up-to-date.png"
$uiFixtureSettingsUpdatesAvailableScreenshotPath = Join-Path $runDir "settings-updates-available.png"
$uiFixtureSettingsOrdinarySearchScreenshotPath = Join-Path $runDir "settings-search-ordinary.png"
$uiFixtureSettingsDeveloperHiddenScreenshotPath = Join-Path $runDir "settings-search-developer-hidden.png"
$uiFixtureSettingsAdvancedRevealScreenshotPath = Join-Path $runDir "settings-advanced-revealed.png"
$feedbackChannelChooserScreenshotPath = Join-Path $runDir "feedback-channel-chooser-focused.png"
$feedbackSharedInputBidiScreenshotPath = Join-Path $runDir "feedback-shared-input-bidi-selection.png"
$feedbackComposerMultilineScreenshotPath = Join-Path $runDir "feedback-composer-multiline.png"
$feedbackComposerSelectedScreenshotPath = Join-Path $runDir "feedback-composer-selected-all.png"
$feedbackComposerDragScreenshotPath = Join-Path $runDir "feedback-composer-reverse-drag.png"
$feedbackNarrowMainScreenshotPath = Join-Path $runDir "feedback-main-window-narrow.png"
$feedbackSoundSliderHoverScreenshotPath = Join-Path $runDir "feedback-sound-slider-hover.png"
$feedbackSoundSliderClickScreenshotPath = Join-Path $runDir "feedback-sound-slider-click.png"
$uiFixtureFinalScreenshotPath = Join-Path $runDir "final-state.png"
$linkPreviewEventsPath = Join-Path $runDir "link-preview-matrix-events.raw.txt"
$linkPreviewSummaryPath = Join-Path $runDir "link-preview-matrix-summary.json"
$linkPreviewProfileRoot = Join-Path $runDir "echo-link-preview-profile"
$linkPreviewReceiptPath = Join-Path $linkPreviewProfileRoot "echo-live-channel-receipt.json"
$linkPreviewReplayPath = Join-Path $runDir "link-preview-matrix-local-replay.jsonl"
$linkPreviewReplayReportPath = Join-Path $runDir "link-preview-matrix-local-replay-report.json"
$linkPreviewNonce = ""

if ($LinkPreviewMatrix) {
    $linkPreviewNonce = [Guid]::NewGuid().ToString("N")
    [void](New-Item -ItemType Directory -Force -Path $linkPreviewProfileRoot)
    # storage_roots owns Echo's log root, so the native marker waiters
    # must tail the profile-routed log instead of the ordinary run root.
    $runtimeLogPath = Join-Path (Join-Path $linkPreviewProfileRoot "logs") "echo.log"
    $localReplay = [ordered]@{
        kind = "message"
        sequence = 1
        channel = $LinkPreviewMatrixChannel
        login = "echolinkpreviewfixture"
        display_name = "EchoLinkPreviewFixture"
        message = [ordered]@{
            id = "echo-link-preview-matrix-local"
            text = "Local replay link preview https://echo.invalid/link-preview-matrix.png"
        }
    }
    [System.IO.File]::WriteAllText(
        $linkPreviewReplayPath,
        (($localReplay | ConvertTo-Json -Compress) + [Environment]::NewLine),
        [System.Text.UTF8Encoding]::new($false)
    )
}

$launchEnvironment = @(
    "ECHO_FPS_PROFILER=1",
    "ECHO_PERF_MONITOR=1",
    "ECHO_LOG_FILE=1",
    "ECHO_LOG_FILE_PATH=$runtimeLogPath",
    "ECHO_LOG_PERF=1",
    "ECHO_LOG_PERF_RENDER=1",
    "ECHO_LOG_PERF_EMOTES=1",
    "ECHO_LOG_IRC=1",
    "ECHO_LOG_EMOTES=1",
    "ECHO_LOG_CACHE=1",
    "ECHO_LOG_NET=1",
    "ECHO_LOG_UI=1",
    "ECHO_BENCH_UI_SMOKE=1",
    "ECHO_BENCH_UI_SMOKE_DELAY_MS=4000",
    "ECHO_BENCH_UI_SMOKE_STEP_DELAY_MS=400",
    "ECHO_BENCH_SCROLL_SMOKE=0",
    "ECHO_BENCH_SCROLL_SMOKE_SEQUENCE=",
    "ECHO_BENCH_SCROLL_SMOKE_DELAY_MS=0",
    "ECHO_BENCH_SCROLL_SMOKE_STEP_DELAY_MS=0"
)
if ($UiFixtureInteractions) {
    $launchEnvironment += "ECHO_BENCH_UI_FIXTURE=1"
}
if ($UiFixtureSettingsOnly) {
    $launchEnvironment += "ECHO_BENCH_UI_FIXTURE_SETTINGS_ONLY=1"
}
if ($FeedbackUiCheckpoints) {
    $launchEnvironment += "ECHO_BENCH_FEEDBACK_UI=1"
}
if ($LinkPreviewMatrix) {
    $launchEnvironment += "ECHO_BENCH_LINK_PREVIEW_MATRIX=1"
    $launchEnvironment += "ECHO_BENCH_LINK_PREVIEW_MATRIX_DELAY_MS=4000"
    $launchEnvironment += "ECHO_BENCH_PROFILE_ROOT=$linkPreviewProfileRoot"
    $launchEnvironment += "ECHO_BENCH_OBSERVATION_NONCE=$linkPreviewNonce"
    $launchEnvironment += "ECHO_BENCH_LIVE_CHANNEL=$LinkPreviewMatrixChannel"
    $launchEnvironment += "ECHO_BENCH_LIVE_CHANNEL_RECEIPT=$linkPreviewReceiptPath"
    $launchEnvironment += "ECHO_CHAT_REPLAY=$linkPreviewReplayPath"
    $launchEnvironment += "ECHO_CHAT_REPLAY_REPORT=$linkPreviewReplayReportPath"
    $launchEnvironment += "ECHO_CHAT_REPLAY_CHANNEL=$LinkPreviewMatrixChannel"
    $launchEnvironment += "ECHO_CHAT_REPLAY_CLEAR_CHANNEL=0"
}
if ($pickerSweepEnabled) {
    $launchEnvironment += "ECHO_BENCH_PICKER_SWEEP_CHANNEL=$pickerSweepChannel"
    $launchEnvironment += "ECHO_BENCH_PICKER_ANIMATION_MODE=always"
    $launchEnvironment += "ECHO_BENCH_PICKER_SWEEP_DWELL_MS=120"
    $launchEnvironment += "ECHO_BENCH_PICKER_SWEEP_SETTLE_MS=4000"
    $launchEnvironment += "ECHO_BENCH_PICKER_SWEEP_OVERLAP_ROWS=3"
    $launchEnvironment += "ECHO_BENCH_PICKER_STABILITY_SETTLE_MS=5000"
    $launchEnvironment += "ECHO_BENCH_PICKER_STABILITY_SAMPLE_MS=120000"
    $launchEnvironment += "ECHO_BENCH_PICKER_STABILITY_TICK_MS=250"
}

$samplerPath = Join-Path $scriptRoot "sample-process.ps1"
$samplerParameters = @{
    App = "echo"
    MetricName = $MetricName
    RawFileName = $rawFileName
    RunDir = $runDir
    LaunchPath = $resolvedLaunchPath
    LaunchWorkingDirectory = $repoRoot
    LaunchEnvironment = $launchEnvironment
    DurationSeconds = $DurationSeconds
    IntervalMilliseconds = $IntervalMilliseconds
    # Leave Echo running until this harness can request a normal window close.
    # A forced sampler stop makes the next launch look like a crash recovery.
    StopLaunchedProcess = $false
}

$samplerJob = Start-Job -ScriptBlock {
    param($ScriptPath, $Parameters)
    & $ScriptPath @Parameters
} -ArgumentList $samplerPath, $samplerParameters

$processName = [System.IO.Path]::GetFileNameWithoutExtension($resolvedLaunchPath)
$windowDeadline = [DateTime]::UtcNow.AddSeconds(30)
$echoProcess = $null
while ([DateTime]::UtcNow -lt $windowDeadline) {
    $echoProcess = Get-Process -Name $processName -ErrorAction SilentlyContinue |
        Where-Object { $_.MainWindowHandle -ne [IntPtr]::Zero } |
        Sort-Object StartTime -Descending |
        Select-Object -First 1
    if ($null -ne $echoProcess) {
        break
    }
    Start-Sleep -Milliseconds 250
}
if ($null -eq $echoProcess) {
    throw "Echo did not expose a main window within 30 seconds."
}
$mainWindowHandle = $echoProcess.MainWindowHandle
$mainRootWindowHandle = [EchoLiveUiNativeMethods]::GetAncestor($mainWindowHandle, 2)
if ($mainRootWindowHandle -eq [IntPtr]::Zero) {
    $mainRootWindowHandle = $mainWindowHandle
}
$popupMonitorOriginalBounds = $null
$popupMonitorTargetDevice = $null

try {
if ($PopupMonitorCheckpoints) {
    if (-not $UiFixtureInteractions) {
        throw "Popup monitor checkpoints require -UiFixtureInteractions."
    }
    Add-Type -AssemblyName System.Windows.Forms
    $originalRect = [EchoLiveUiNativeMethods+Rect]::new()
    if (-not [EchoLiveUiNativeMethods]::GetWindowRect($mainRootWindowHandle, [ref]$originalRect)) {
        throw "Could not read Echo's main-window bounds before the popup monitor checkpoint."
    }
    $popupMonitorOriginalBounds = $originalRect
    $currentScreen = [System.Windows.Forms.Screen]::FromHandle($mainRootWindowHandle)
    $targetScreen = @(
        [System.Windows.Forms.Screen]::AllScreens |
            Where-Object { $_.DeviceName -ne $currentScreen.DeviceName }
    ) | Select-Object -First 1
    if ($null -eq $targetScreen) {
        throw "Popup monitor checkpoints require at least two active displays."
    }
    $targetArea = $targetScreen.WorkingArea
    $originalWidth = $originalRect.Right - $originalRect.Left
    $originalHeight = $originalRect.Bottom - $originalRect.Top
    $targetWidth = [Math]::Min($originalWidth, [Math]::Max(640, $targetArea.Width - 120))
    $targetHeight = [Math]::Min($originalHeight, [Math]::Max(480, $targetArea.Height - 120))
    $targetX = $targetArea.Left + [Math]::Max(0, [Math]::Floor(($targetArea.Width - $targetWidth) / 2))
    $targetY = $targetArea.Top + [Math]::Max(0, [Math]::Floor(($targetArea.Height - $targetHeight) / 2))
    if (-not [EchoLiveUiNativeMethods]::SetWindowPos(
        $mainRootWindowHandle,
        [IntPtr]::Zero,
        $targetX,
        $targetY,
        $targetWidth,
        $targetHeight,
        0x0014
    )) {
        throw "Could not move Echo to '$($targetScreen.DeviceName)' for the popup monitor checkpoint."
    }
    Start-Sleep -Milliseconds 500
    $movedScreen = [System.Windows.Forms.Screen]::FromHandle($mainRootWindowHandle)
    if ($movedScreen.DeviceName -ne $targetScreen.DeviceName) {
        throw "Echo remained on '$($movedScreen.DeviceName)' after targeting '$($targetScreen.DeviceName)'."
    }
    $popupMonitorTargetDevice = $targetScreen.DeviceName
    [System.IO.File]::WriteAllLines(
        $popupMonitorPath,
        @(
            "stage=main_moved from=$($currentScreen.DeviceName) to=$popupMonitorTargetDevice bounds=$targetX,$targetY,$targetWidth,$targetHeight passed=true"
        )
    )
}
# The UI fixture path uses renderer-owned checkpoints rather than a wall-clock
# screenshot delay. This lets each saved image correspond to the named state
# that Rust has confirmed is on screen. Without UI fixture interactions the
# suite keeps its timed screenshot behavior.
if ($LinkPreviewMatrix) {
    foreach ($display in @("none", "manual", "always")) {
        foreach ($thumbnail in @("none", "small", "medium", "large")) {
            [void](Wait-UiFixtureMarker `
                -RuntimeLogPath $runtimeLogPath `
                -Pattern "bench\.link_preview_matrix visual_ready display=$display thumbnail=$thumbnail stage=initial(?:\s|$)" `
                -Description "link-preview $display/$thumbnail initial state" `
                -EchoProcess $echoProcess `
                -MarkerNamespace "bench\.link_preview_matrix")
            Save-WindowScreenshot `
                -Path (Join-Path $runDir "link-preview-$display-$thumbnail-initial.png") `
                -WindowHandle $mainRootWindowHandle

            if ($display -eq "manual") {
                [void](Wait-UiFixtureMarker `
                    -RuntimeLogPath $runtimeLogPath `
                    -Pattern "bench\.link_preview_matrix visual_ready display=manual thumbnail=$thumbnail stage=shown(?:\s|$)" `
                    -Description "link-preview manual/$thumbnail shown state" `
                    -EchoProcess $echoProcess `
                    -MarkerNamespace "bench\.link_preview_matrix")
                Save-WindowScreenshot `
                    -Path (Join-Path $runDir "link-preview-manual-$thumbnail-shown.png") `
                    -WindowHandle $mainRootWindowHandle
                [void](Wait-UiFixtureMarker `
                    -RuntimeLogPath $runtimeLogPath `
                    -Pattern "bench\.link_preview_matrix visual_ready display=manual thumbnail=$thumbnail stage=hidden(?:\s|$)" `
                    -Description "link-preview manual/$thumbnail hidden state" `
                    -EchoProcess $echoProcess `
                    -MarkerNamespace "bench\.link_preview_matrix")
                Save-WindowScreenshot `
                    -Path (Join-Path $runDir "link-preview-manual-$thumbnail-hidden.png") `
                    -WindowHandle $mainRootWindowHandle
            }
        }
    }
}
if ($UiFixtureInteractions) {
    [void](Wait-UiFixtureMarker `
        -RuntimeLogPath $runtimeLogPath `
        -Pattern "bench\.ui_fixture fixture_seeded tabs=8 splits=2 offline_channel=echo-offline-fixture live_channel=echo-live-fixture roomstate_labels=5" `
        -Description "fixture seed" `
        -EchoProcess $echoProcess)
    [void](Wait-UiFixtureMarker `
        -RuntimeLogPath $runtimeLogPath `
        -Pattern "bench\.ui_fixture semantic_snapshot offline_status=OFFLINE offline_title=true offline_category=true live_status=LIVE live_title=true live_category=true live_viewers=12345 roomstate_labels=5 selected_tabs=1 highlighted_tabs=2 new_message_tabs=1 unread_fallback_tabs=1 live_tabs=2 long_title_tabs=1" `
        -Description "header and tab semantic snapshot" `
        -EchoProcess $echoProcess)
    [void](Wait-UiFixtureMarker `
        -RuntimeLogPath $runtimeLogPath `
        -Pattern "bench\.ui_fixture surface=activity stage=seeded mention_present=true whisper_present=true whisper_unread=1 event_present=true" `
        -Description "Activity fixture seed" `
        -EchoProcess $echoProcess)

    [void](Wait-UiFixtureMarker `
        -RuntimeLogPath $runtimeLogPath `
        -Pattern "bench\.ui_fixture visual_ready checkpoint=header_tabs(?:\s|$)" `
        -Description "header_tabs visual checkpoint" `
        -EchoProcess $echoProcess)
    $echoProcess.Refresh()
    Save-WindowScreenshot `
        -Path $uiFixtureHeaderTabsScreenshotPath `
        -WindowHandle $mainWindowHandle

    [void](Wait-UiFixtureMarker `
        -RuntimeLogPath $runtimeLogPath `
        -Pattern "bench\.ui_fixture visual_ready checkpoint=channel_popup_wide title=Echo_-_#echo-offline-fixture passed=true(?:\s|$)" `
        -Description "wide channel popup visual checkpoint" `
        -EchoProcess $echoProcess)
    $channelPopupTitlePrefix = "Echo - #echo-offline-fixture"
    $channelPopupDeadline = [DateTime]::UtcNow.AddSeconds(30)
    $channelPopupWindow = [IntPtr]::Zero
    while ([DateTime]::UtcNow -lt $channelPopupDeadline) {
        $channelPopupWindow = Find-ProcessWindowByTitlePrefix `
            -ProcessId $echoProcess.Id `
            -TitlePrefix $channelPopupTitlePrefix
        if ($channelPopupWindow -ne [IntPtr]::Zero) {
            break
        }
        Start-Sleep -Milliseconds 50
    }
    if ($channelPopupWindow -eq [IntPtr]::Zero) {
        throw "The UI fixture channel popup '$channelPopupTitlePrefix' did not open within 30 seconds."
    }
    if ($channelPopupWindow -eq $mainWindowHandle -or $channelPopupWindow -eq $mainRootWindowHandle) {
        throw "The UI fixture channel popup unexpectedly resolved to Echo's main window."
    }
    if ($PopupMonitorCheckpoints) {
        $mainScreen = [System.Windows.Forms.Screen]::FromHandle($mainRootWindowHandle)
        $popupScreen = [System.Windows.Forms.Screen]::FromHandle($channelPopupWindow)
        $sameMonitor = $mainScreen.DeviceName -eq $popupScreen.DeviceName -and
            $mainScreen.DeviceName -eq $popupMonitorTargetDevice
        [System.IO.File]::AppendAllText(
            $popupMonitorPath,
            "stage=channel_popup main=$($mainScreen.DeviceName) popup=$($popupScreen.DeviceName) expected=$popupMonitorTargetDevice passed=$($sameMonitor.ToString().ToLowerInvariant())`r`n"
        )
        if (-not $sameMonitor) {
            throw "The channel popup opened on '$($popupScreen.DeviceName)' while Echo was on '$($mainScreen.DeviceName)'."
        }
    }

    [void](Set-UiFixtureWindowSize `
        -WindowHandle $channelPopupWindow `
        -TargetName "wide offline channel popup" `
        -Width 760 `
        -Height 700 `
        -MinimumObservedWidth 740 `
        -MaximumObservedWidth 800)
    Save-WindowScreenshot `
        -Path $uiFixtureChannelPopupWideScreenshotPath `
        -WindowHandle $channelPopupWindow

    [void](Set-UiFixtureWindowSize `
        -WindowHandle $channelPopupWindow `
        -TargetName "narrow offline channel popup" `
        -Width 360 `
        -Height 700 `
        -MinimumObservedWidth 350 `
        -MaximumObservedWidth 400)
    [void](Wait-UiFixtureMarker `
        -RuntimeLogPath $runtimeLogPath `
        -Pattern "bench\.ui_fixture visual_ready checkpoint=channel_popup_narrow title=Echo_-_#echo-offline-fixture passed=true(?:\s|$)" `
        -Description "narrow channel popup visual checkpoint" `
        -EchoProcess $echoProcess)
    Save-WindowScreenshot `
        -Path $uiFixtureChannelPopupNarrowScreenshotPath `
        -WindowHandle $channelPopupWindow
    if ($FeedbackUiCheckpoints) {
        $sharedInputBidiTarget = Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture visual_ready checkpoint=feedback_shared_input_bidi_target x=-?\d+(?:\.\d+)? y=-?\d+(?:\.\d+)? width=\d+(?:\.\d+)? height=\d+(?:\.\d+)? text_len=11 selection_len=0 target_selection_start=\d+ target_selection_end=\d+ target_selection_start_affinity=(?:leading|trailing) target_selection_end_affinity=(?:leading|trailing) selection_start_local_x=\d+(?:\.\d+)? selection_end_local_x=\d+(?:\.\d+)? native_pointer_drag=true sent=false passed=true(?:\s|$)" `
            -Description "shared Input mixed-direction selection target" `
            -EchoProcess $echoProcess
        $targetSelectionStart = [int](Get-UiFixtureMarkerNumber `
            -Line $sharedInputBidiTarget `
            -Field "target_selection_start")
        $targetSelectionEnd = [int](Get-UiFixtureMarkerNumber `
            -Line $sharedInputBidiTarget `
            -Field "target_selection_end")
        Invoke-UiFixtureKnownBidiSelection `
            -WindowHandle $channelPopupWindow `
            -MarkerLine $sharedInputBidiTarget `
            -TargetName "fixture shared Input mixed-direction text"
        $sharedInputBidiSelectionPattern = "bench\.ui_fixture surface=shared_input stage=bidi_pointer_selection input=production_shared_input native_pointer_drag=true text_len=11 cursor=\d+ cursor_affinity=(?:leading|trailing) target_selection_start=$targetSelectionStart target_selection_end=$targetSelectionEnd target_selection_start_affinity=(?:leading|trailing) target_selection_end_affinity=(?:leading|trailing) selection_start=$targetSelectionStart selection_end=$targetSelectionEnd selection_span_count=[2-9]\d* selection_span_geometry=-?\d+(?:\.\d+)?:\d+(?:\.\d+)?(?:,-?\d+(?:\.\d+)?:\d+(?:\.\d+)?)+ sent=false passed=true(?:\s|$)"
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern $sharedInputBidiSelectionPattern `
            -Description "shared Input known-range native multi-span selection" `
            -EchoProcess $echoProcess)
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture visual_ready checkpoint=feedback_shared_input_bidi_selected passed=true(?:\s|$)" `
            -Description "shared Input mixed-direction visual checkpoint" `
            -EchoProcess $echoProcess)
        Save-WindowScreenshot `
            -Path $feedbackSharedInputBidiScreenshotPath `
            -WindowHandle $channelPopupWindow
    }
    [void](Wait-UiFixtureMarker `
        -RuntimeLogPath $runtimeLogPath `
        -Pattern "bench\.ui_fixture state=channel_popup open=false passed=true(?:\s|$)" `
        -Description "channel popup close state" `
        -EchoProcess $echoProcess)
    Wait-ProcessWindowClosedByTitlePrefix `
        -ProcessId $echoProcess.Id `
        -TitlePrefix $channelPopupTitlePrefix

    $popupDestroyDeadline = [DateTime]::UtcNow.AddSeconds(10)
    while (
        [EchoLiveUiNativeMethods]::IsWindow($channelPopupWindow) -and
        [DateTime]::UtcNow -lt $popupDestroyDeadline
    ) {
        Start-Sleep -Milliseconds 50
    }
    if ([EchoLiveUiNativeMethods]::IsWindow($channelPopupWindow)) {
        throw "The UI fixture popup removed its title/registry state but its native HWND was not destroyed within 10 seconds."
    }
    if ($PopupMonitorCheckpoints -and $null -ne $popupMonitorOriginalBounds) {
        Restore-UiFixtureWindowBounds `
            -WindowHandle $mainRootWindowHandle `
            -Bounds $popupMonitorOriginalBounds `
            -TargetName "popup monitor main window"
        [System.IO.File]::AppendAllText(
            $popupMonitorPath,
            "stage=main_restored passed=true`r`n"
        )
        $popupMonitorOriginalBounds = $null
    }

    # The fixture popup is created focused. After it closes, Windows may put
    # an unrelated desktop window above Echo instead of restoring the original
    # main HWND. Reactivate the launch-pinned main window, then retain the
    # existing WindowFromPoint occlusion refusal for every pointer target.
    # SetForegroundWindow can be denied by Windows focus-stealing policy even
    # for a process launched by this harness, and the currently active desktop
    # app may immediately reclaim an ordinary HWND_TOP raise. Keep this
    # short-lived harness process topmost only for its bounded tab interactions.
    # Echo is closed by the harness and this OS-only state is never persisted.
    $swpNoSize = 0x0001
    $swpNoMove = 0x0002
    $swpShowWindow = 0x0040
    if (-not [EchoLiveUiNativeMethods]::SetWindowPos(
        $mainRootWindowHandle,
        [IntPtr]::new(-1),
        0,
        0,
        0,
        0,
        $swpNoSize -bor $swpNoMove -bor $swpShowWindow
    )) {
        throw "Could not restore Echo's main window Z-order after closing the UI fixture popup."
    }
    [void][EchoLiveUiNativeMethods]::SetForegroundWindow($mainRootWindowHandle)
    Start-Sleep -Milliseconds 250

    # Completion prefixes and cursor positions are prepared by Echo's
    # disposable in-process fixture. Acceptance and cycling then cross the real
    # Windows -> GPUI keybinding boundary with Tab/Shift+Tab. No text, Enter,
    # message-send, or moderation input is injected.
    if ($CursorStabilityCheckpoints) {
        Set-NativeCursorTargetOverWindow `
            -WindowHandle $mainRootWindowHandle
    }
    $completionSuggestionStages = @(
        @{
            Target = "main"
            Mode = "emote"
            ScreenshotPath = $uiFixtureCompletionMainEmoteScreenshotPath
        },
        @{
            Target = "main"
            Mode = "mention"
            ScreenshotPath = $uiFixtureCompletionMainMentionScreenshotPath
        },
        @{
            Target = "watching"
            Mode = "emote"
            ScreenshotPath = $uiFixtureCompletionWatchingEmoteScreenshotPath
        },
        @{
            Target = "thread"
            Mode = "emote"
            ScreenshotPath = $uiFixtureCompletionThreadEmoteScreenshotPath
        },
        @{
            Target = "thread"
            Mode = "mention"
            ScreenshotPath = $uiFixtureCompletionThreadMentionScreenshotPath
        }
    )

    foreach ($completionStage in $completionSuggestionStages) {
        $completionTarget = $completionStage.Target
        $completionMode = $completionStage.Mode
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture surface=completion target=$completionTarget mode=$completionMode stage=suggestions .*focused=true .*passed=true(?:\s|$)" `
            -Description "$completionTarget $completionMode completion suggestions" `
            -EchoProcess $echoProcess)
        Start-Sleep -Milliseconds 250
        Save-WindowScreenshot `
            -Path $completionStage.ScreenshotPath `
            -WindowHandle $mainWindowHandle `
            -VisibleSurface
        Send-NativeTabKey -WindowHandle $mainRootWindowHandle

        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture surface=completion target=$completionTarget mode=$completionMode stage=suggestion_accept key=native_tab .*sent=false .*passed=true(?:\s|$)" `
            -Description "$completionTarget $completionMode suggestion acceptance" `
            -EchoProcess $echoProcess)
        if ($CursorStabilityCheckpoints) {
            Assert-NativeCursorStable `
                -Context "$completionTarget $completionMode suggestion acceptance"
        }

        foreach ($completionStep in 1..4) {
            [void](Wait-UiFixtureMarker `
                -RuntimeLogPath $runtimeLogPath `
                -Pattern "bench\.ui_fixture surface=completion target=$completionTarget mode=$completionMode stage=inline_ready step=$completionStep .*key=native_tab .*focused=true .*passed=true(?:\s|$)" `
                -Description "$completionTarget $completionMode inline Tab step $completionStep readiness" `
                -EchoProcess $echoProcess)
            Send-NativeTabKey `
                -WindowHandle $mainRootWindowHandle `
                -Reverse:($completionStep -ge 3)
            [void](Wait-UiFixtureMarker `
                -RuntimeLogPath $runtimeLogPath `
                -Pattern "bench\.ui_fixture surface=completion target=$completionTarget mode=$completionMode stage=inline_cycle step=$completionStep key=native_tab .*sent=false .*passed=true(?:\s|$)" `
                -Description "$completionTarget $completionMode inline Tab step $completionStep result" `
                -EchoProcess $echoProcess)
            if ($CursorStabilityCheckpoints) {
                Assert-NativeCursorStable `
                    -Context "$completionTarget $completionMode inline Tab step $completionStep"
            }

            if ($completionStep -eq 4 -and $completionMode -eq "emote") {
                $cycleScreenshotPath = if ($completionTarget -eq "main") {
                    $uiFixtureCompletionMainCycleScreenshotPath
                } elseif ($completionTarget -eq "watching") {
                    $uiFixtureCompletionWatchingCycleScreenshotPath
                } else {
                    $uiFixtureCompletionThreadCycleScreenshotPath
                }
                Start-Sleep -Milliseconds 250
                Save-WindowScreenshot `
                    -Path $cycleScreenshotPath `
                    -WindowHandle $mainWindowHandle `
                    -VisibleSurface
            }
        }
    }

    [void](Wait-UiFixtureMarker `
        -RuntimeLogPath $runtimeLogPath `
        -Pattern "bench\.ui_fixture surface=completion stage=complete .*sent=false .*passed=true(?:\s|$)" `
        -Description "completion interaction cleanup" `
        -EchoProcess $echoProcess)
    if ($CursorStabilityCheckpoints) {
        Assert-NativeCursorStableWhileEchoUnfocused `
            -MainWindowHandle $mainRootWindowHandle
    }

    # Settings search is driven in-process through the same production state
    # controller used by the modal. This outer harness only captures states
    # after Echo has verified that no persisted AppSettings value changed.
    foreach ($settingsCheckpoint in @(
        @{
            Name = "settings_default"
            Description = "default settings surface"
            Pattern = "bench\.ui_fixture surface=settings stage=open open=true search_focused=true query_empty=true advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true(?:\s|$)"
            Path = $uiFixtureSettingsDefaultScreenshotPath
        },
        @{
            Name = "settings_updates_up_to_date"
            Description = "up-to-date settings update state"
            Pattern = "bench\.ui_fixture surface=settings section=updates destination=General state=up_to_date installed=\S+ latest=\S+ persisted_unchanged=true passed=true(?:\s|$)"
            Path = $uiFixtureSettingsUpdatesUpToDateScreenshotPath
        },
        @{
            Name = "settings_updates_available"
            Description = "available settings update state"
            Pattern = "bench\.ui_fixture surface=settings section=updates destination=General state=available installed=\S+ available=\S+ persisted_unchanged=true passed=true(?:\s|$)"
            Path = $uiFixtureSettingsUpdatesAvailableScreenshotPath
        },
        @{
            Name = "settings_search_ordinary"
            Description = "ordinary settings search"
            Pattern = "bench\.ui_fixture surface=settings stage=query kind=ordinary query=emote selected=Chat result_count=\d+ no_results=false advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true(?:\s|$)"
            Path = $uiFixtureSettingsOrdinarySearchScreenshotPath
        },
        @{
            Name = "settings_search_developer_hidden"
            Description = "developer settings hidden search"
            Pattern = "bench\.ui_fixture surface=settings stage=query kind=developer_hidden query=fps result_count=0 no_results=true advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true(?:\s|$)"
            Path = $uiFixtureSettingsDeveloperHiddenScreenshotPath
        },
        @{
            Name = "settings_advanced_revealed"
            Description = "explicit advanced settings reveal"
            Pattern = "bench\.ui_fixture surface=settings stage=reveal query=fps selected=Advanced page=advanced anchor=diagnostics result_count=\d+ no_results=false advanced_revealed=true developer_visible=true persisted_unchanged=true passed=true(?:\s|$)"
            Path = $uiFixtureSettingsAdvancedRevealScreenshotPath
        }
    )) {
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern $settingsCheckpoint.Pattern `
            -Description $settingsCheckpoint.Description `
            -EchoProcess $echoProcess)
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture visual_ready checkpoint=$($settingsCheckpoint.Name) passed=true(?:\s|$)" `
            -Description "$($settingsCheckpoint.Description) visual checkpoint" `
            -EchoProcess $echoProcess)
        Start-Sleep -Milliseconds 250
        Save-WindowScreenshot `
            -Path $settingsCheckpoint.Path `
            -WindowHandle $mainWindowHandle `
            -VisibleSurface
    }

    [void](Wait-UiFixtureMarker `
        -RuntimeLogPath $runtimeLogPath `
        -Pattern "bench\.ui_fixture surface=settings stage=empty query_empty=true result_count=\d+ no_results=false advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true(?:\s|$)" `
        -Description "empty settings query safety" `
        -EchoProcess $echoProcess)
    [void](Wait-UiFixtureMarker `
        -RuntimeLogPath $runtimeLogPath `
        -Pattern "bench\.ui_fixture surface=settings stage=complete open=false query_empty=true advanced_revealed=false persisted_unchanged=true passed=true(?:\s|$)" `
        -Description "settings interaction cleanup" `
        -EchoProcess $echoProcess)

    if ($FeedbackUiCheckpoints) {
        # The settings-open marker is intentionally asserted again as part of
        # this feedback contract: it is the source of truth for search focus,
        # and the harness never injects text to manufacture that state.
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture surface=settings stage=open open=true search_focused=true query_empty=true advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true(?:\s|$)" `
            -Description "feedback settings search focus" `
            -EchoProcess $echoProcess)

        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture surface=channel_chooser stage=open open=true input_focused=true query_empty=true placeholder_present=true sent=false passed=true(?:\s|$)" `
            -Description "channel chooser autofocus" `
            -EchoProcess $echoProcess)
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture surface=channel_chooser stage=stable open=true input_focused=true query_empty=true placeholder_unchanged=true sent=false passed=true(?:\s|$)" `
            -Description "channel chooser placeholder stability" `
            -EchoProcess $echoProcess)
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture visual_ready checkpoint=feedback_channel_chooser passed=true(?:\s|$)" `
            -Description "channel chooser visual checkpoint" `
            -EchoProcess $echoProcess)
        Save-WindowScreenshot `
            -Path $feedbackChannelChooserScreenshotPath `
            -WindowHandle $mainWindowHandle `
            -VisibleSurface
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture surface=channel_chooser stage=complete open=false sent=false passed=true(?:\s|$)" `
            -Description "channel chooser cleanup" `
            -EchoProcess $echoProcess)

        $composerMultilineLine = Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture surface=composer stage=multiline prepared_in_process=true native_text_input=false line_count=\d+ composer_height=\d+(?:\.\d+)? text_height=\d+(?:\.\d+)? action_height=\d+(?:\.\d+)? overlap_safe=true clipped=false sent=false passed=true(?:\s|$)" `
            -Description "multiline composer geometry" `
            -EchoProcess $echoProcess
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture visual_ready checkpoint=feedback_composer_multiline passed=true(?:\s|$)" `
            -Description "multiline composer visual checkpoint" `
            -EchoProcess $echoProcess)
        Save-WindowScreenshot `
            -Path $feedbackComposerMultilineScreenshotPath `
            -WindowHandle $mainWindowHandle `
            -VisibleSurface

        $composerSelectionTarget = Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture visual_ready checkpoint=feedback_composer_selection_target x=-?\d+(?:\.\d+)? y=-?\d+(?:\.\d+)? width=\d+(?:\.\d+)? height=\d+(?:\.\d+)? text_len=\d+ selection_len=0 native_mouse=true sent=false passed=true(?:\s|$)" `
            -Description "multiline composer selection target" `
            -EchoProcess $echoProcess
        Invoke-UiFixtureSafeTripleClick `
            -WindowHandle $mainWindowHandle `
            -MarkerLine $composerSelectionTarget `
            -TargetName "fixture multiline composer"
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture surface=composer stage=triple_click_select_all native_mouse=true text_len=\d+ selection_start=0 selection_end=\d+ selection_visible=true sent=false passed=true(?:\s|$)" `
            -Description "multiline composer native triple-click selection" `
            -EchoProcess $echoProcess)
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture visual_ready checkpoint=feedback_composer_selected_all passed=true(?:\s|$)" `
            -Description "multiline composer selected visual checkpoint" `
            -EchoProcess $echoProcess)
        Save-WindowScreenshot `
            -Path $feedbackComposerSelectedScreenshotPath `
            -WindowHandle $mainWindowHandle `
            -VisibleSurface
        $composerDragTarget = Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture visual_ready checkpoint=feedback_composer_drag_target x=-?\d+(?:\.\d+)? y=-?\d+(?:\.\d+)? width=\d+(?:\.\d+)? height=\d+(?:\.\d+)? text_len=\d+ selection_len=0 native_mouse=true sent=false passed=true(?:\s|$)" `
            -Description "multiline composer reverse-drag target" `
            -EchoProcess $echoProcess
        Invoke-UiFixtureSafeReverseDrag `
            -WindowHandle $mainWindowHandle `
            -MarkerLine $composerDragTarget `
            -TargetName "fixture multiline composer"
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture surface=composer stage=reverse_drag native_mouse=true text_len=\d+ selection_start=\d+ selection_end=\d+ cursor=\d+ reversed=true selection_visible=true sent=false passed=true(?:\s|$)" `
            -Description "multiline composer native reverse drag" `
            -EchoProcess $echoProcess)
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture visual_ready checkpoint=feedback_composer_reverse_drag passed=true(?:\s|$)" `
            -Description "multiline composer reverse-drag visual checkpoint" `
            -EchoProcess $echoProcess)
        Save-WindowScreenshot `
            -Path $feedbackComposerDragScreenshotPath `
            -WindowHandle $mainWindowHandle `
            -VisibleSurface
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture surface=composer stage=selection_cleanup selection_cleared=true sent=false passed=true(?:\s|$)" `
            -Description "multiline composer selection cleanup" `
            -EchoProcess $echoProcess)

        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture surface=main_window stage=narrow_ready target_width=520 sent=false passed=true(?:\s|$)" `
            -Description "narrow main-window readiness" `
            -EchoProcess $echoProcess)
        $mainWindowOriginalBounds = [EchoLiveUiNativeMethods+Rect]::new()
        if (-not [EchoLiveUiNativeMethods]::GetWindowRect(
            $mainRootWindowHandle,
            [ref]$mainWindowOriginalBounds
        )) {
            throw "Could not capture Echo's main-window bounds before the narrow feedback checkpoint."
        }
        try {
            [void](Set-UiFixtureWindowSize `
                -WindowHandle $mainRootWindowHandle `
                -TargetName "narrow feedback main window" `
                -Width 520 `
                -Height 700 `
                -MinimumObservedWidth 500 `
                -MaximumObservedWidth 560)
            [void](Wait-UiFixtureMarker `
                -RuntimeLogPath $runtimeLogPath `
                -Pattern "bench\.ui_fixture surface=main_window stage=narrow width=\d+(?:\.\d+)? layout_safe=true header_overlap=false composer_overlap=false sent=false passed=true(?:\s|$)" `
                -Description "narrow main-window layout" `
                -EchoProcess $echoProcess)
            [void](Wait-UiFixtureMarker `
                -RuntimeLogPath $runtimeLogPath `
                -Pattern "bench\.ui_fixture visual_ready checkpoint=feedback_main_window_narrow passed=true(?:\s|$)" `
                -Description "narrow main-window visual checkpoint" `
                -EchoProcess $echoProcess)
            Save-WindowScreenshot `
                -Path $feedbackNarrowMainScreenshotPath `
                -WindowHandle $mainWindowHandle `
                -VisibleSurface
        } finally {
            Restore-UiFixtureWindowBounds `
                -WindowHandle $mainRootWindowHandle `
                -Bounds $mainWindowOriginalBounds `
                -TargetName "feedback main window"
        }
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture surface=main_window stage=restored sent=false passed=true(?:\s|$)" `
            -Description "main-window geometry restoration" `
            -EchoProcess $echoProcess)

        $soundSliderTarget = Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture visual_ready checkpoint=feedback_sound_slider_target x=-?\d+(?:\.\d+)? y=-?\d+(?:\.\d+)? width=\d+(?:\.\d+)? height=\d+(?:\.\d+)? value_before=\d+(?:\.\d+)? enabled=true persistence_suppressed=true sent=false passed=true(?:\s|$)" `
            -Description "sound slider target" `
            -EchoProcess $echoProcess
        [void](Move-CursorToUiFixtureTarget `
            -WindowHandle $mainWindowHandle `
            -MarkerLine $soundSliderTarget `
            -TargetName "notification sound slider")
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture surface=settings stage=sound_slider_hover hovered=true feedback_visible=true value=\d+(?:\.\d+)? persistence_suppressed=true sent=false passed=true(?:\s|$)" `
            -Description "sound slider hover feedback" `
            -EchoProcess $echoProcess)
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture visual_ready checkpoint=feedback_sound_slider_hover passed=true(?:\s|$)" `
            -Description "sound slider hover visual checkpoint" `
            -EchoProcess $echoProcess)
        Save-WindowScreenshot `
            -Path $feedbackSoundSliderHoverScreenshotPath `
            -WindowHandle $mainWindowHandle `
            -VisibleSurface

        Invoke-UiFixtureSafeLeftClick `
            -WindowHandle $mainWindowHandle `
            -MarkerLine $soundSliderTarget `
            -TargetName "notification sound slider"
        $soundSliderClickLine = Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture surface=settings stage=sound_slider_click clicked=true value_after=\d+(?:\.\d+)? value_changed=true feedback_visible=true persistence_suppressed=true sent=false passed=true(?:\s|$)" `
            -Description "sound slider click feedback" `
            -EchoProcess $echoProcess
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture visual_ready checkpoint=feedback_sound_slider_click passed=true(?:\s|$)" `
            -Description "sound slider click visual checkpoint" `
            -EchoProcess $echoProcess)
        Save-WindowScreenshot `
            -Path $feedbackSoundSliderClickScreenshotPath `
            -WindowHandle $mainWindowHandle `
            -VisibleSurface
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture surface=settings stage=sound_slider_complete value_restored=true persisted_unchanged=true sent=false passed=true(?:\s|$)" `
            -Description "sound slider restoration" `
            -EchoProcess $echoProcess)
    }

    if ($UiFixtureSettingsOnly) {
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture surface=settings stage=acceptance_complete scope=settings_only persisted_unchanged=true fixture_clean=true passed=true(?:\s|$)" `
            -Description "settings-only acceptance completion" `
            -EchoProcess $echoProcess)
    }

    if (-not $UiFixtureSettingsOnly) {
    $tabHoverTarget = Wait-UiFixtureMarker `
        -RuntimeLogPath $runtimeLogPath `
        -Pattern "bench\.ui_fixture visual_ready checkpoint=tab_hover target_idx=4 x=-?\d+(?:\.\d+)? y=-?\d+(?:\.\d+)? width=\d+(?:\.\d+)? height=\d+(?:\.\d+)?" `
        -Description "tab_hover target" `
        -EchoProcess $echoProcess
    [void](Move-CursorToUiFixtureTarget `
        -WindowHandle $mainWindowHandle `
        -MarkerLine $tabHoverTarget `
        -TargetName "tab close hover")
    [void](Wait-UiFixtureMarker `
        -RuntimeLogPath $runtimeLogPath `
        -Pattern "bench\.ui_fixture interaction=tab_hover target_idx=4 hovered_idx=4 close_visible=true" `
        -Description "tab hover confirmation" `
        -EchoProcess $echoProcess)
    Save-WindowScreenshot `
        -Path $uiFixtureTabHoverScreenshotPath `
        -WindowHandle $mainWindowHandle

    $dropdownTarget = Wait-UiFixtureMarker `
        -RuntimeLogPath $runtimeLogPath `
        -Pattern "bench\.ui_fixture visual_ready checkpoint=dropdown_button x=-?\d+(?:\.\d+)? y=-?\d+(?:\.\d+)? width=\d+(?:\.\d+)? height=\d+(?:\.\d+)? overflow_count=\d+" `
        -Description "tab dropdown button target" `
        -EchoProcess $echoProcess
    Invoke-UiFixtureSafeLeftClick `
        -WindowHandle $mainWindowHandle `
        -MarkerLine $dropdownTarget `
        -TargetName "tab overflow button"
    [void](Wait-UiFixtureMarker `
        -RuntimeLogPath $runtimeLogPath `
        -Pattern "bench\.ui_fixture interaction=dropdown_click open=true overflow_count=\d+" `
        -Description "tab dropdown click confirmation" `
        -EchoProcess $echoProcess)
    [void](Wait-UiFixtureMarker `
        -RuntimeLogPath $runtimeLogPath `
        -Pattern "bench\.ui_fixture visual_ready checkpoint=dropdown_open(?:\s|$)" `
        -Description "dropdown_open visual checkpoint" `
        -EchoProcess $echoProcess)
    Save-WindowScreenshot `
        -Path $uiFixtureTabDropdownScreenshotPath `
        -WindowHandle $mainWindowHandle

    [void](Wait-UiFixtureMarker `
        -RuntimeLogPath $runtimeLogPath `
        -Pattern "bench\.ui_fixture visual_ready checkpoint=multiline row_count=\d+ allocated_rows=\d+ tab_bar_height=\d+(?:\.\d+)?" `
        -Description "multiline tab visual checkpoint" `
        -EchoProcess $echoProcess)
    Save-WindowScreenshot `
        -Path $uiFixtureTabMultilineScreenshotPath `
        -WindowHandle $mainWindowHandle

    foreach ($activityCheckpoint in @(
        @{ Name = "activity_all"; Path = $uiFixtureActivityAllScreenshotPath },
        @{ Name = "activity_mentions"; Path = $uiFixtureActivityMentionsScreenshotPath },
        @{ Name = "activity_whispers"; Path = $uiFixtureActivityWhispersScreenshotPath },
        @{ Name = "activity_events"; Path = $uiFixtureActivityEventsScreenshotPath }
    )) {
        [void](Wait-UiFixtureMarker `
            -RuntimeLogPath $runtimeLogPath `
            -Pattern "bench\.ui_fixture visual_ready checkpoint=$($activityCheckpoint.Name) passed=true(?:\s|$)" `
            -Description "$($activityCheckpoint.Name) visual checkpoint" `
            -EchoProcess $echoProcess)
        Save-WindowScreenshot `
            -Path $activityCheckpoint.Path `
            -WindowHandle $mainWindowHandle
    }

    [void](Wait-UiFixtureMarker `
        -RuntimeLogPath $runtimeLogPath `
        -Pattern "bench\.ui_fixture visual_ready checkpoint=final(?:\s|$)" `
        -Description "final visual checkpoint" `
        -EchoProcess $echoProcess)
    Save-WindowScreenshot `
        -Path $uiFixtureFinalScreenshotPath `
        -WindowHandle $mainWindowHandle
    Save-WindowScreenshot `
        -Path $screenshotPath `
        -WindowHandle $mainWindowHandle
    }
} else {
    # Capture after metrics opens and while navigation/scroll actions are
    # running, before picker/modal stages can obscure the dashboard.
    Start-Sleep -Seconds $ScreenshotDelaySeconds
    $echoProcess.Refresh()
    Save-WindowScreenshot -Path $screenshotPath -WindowHandle $mainWindowHandle
}

    if ($pickerSweepEnabled) {
        $pickerWindowDeadline = [DateTime]::UtcNow.AddSeconds(30)
        $pickerWindow = [IntPtr]::Zero
        while ([DateTime]::UtcNow -lt $pickerWindowDeadline) {
            $pickerWindow = Find-ProcessWindowByTitlePrefix `
                -ProcessId $echoProcess.Id `
                -TitlePrefix "Emotes - $pickerSweepChannel"
            if ($pickerWindow -ne [IntPtr]::Zero) {
                break
            }
            Start-Sleep -Milliseconds 250
        }
        if ($pickerWindow -eq [IntPtr]::Zero) {
            throw "The emote-picker sweep window for '$pickerSweepChannel' did not open within 30 seconds."
        }

        # A capture taken as soon as the popout appears can still contain
        # ordinary first-load placeholders. Wait for the in-process stationary
        # gate to prove the settled viewport is stable, then capture that exact
        # post-hydration state while the popout remains open.
        # Allow one bounded live-catalog retry plus the 120-second readiness and
        # 30-second post-readiness continuity ceilings. The in-process driver
        # owns the stricter failure markers; this outer watchdog only prevents
        # a genuinely stranded GUI run.
        $pickerStabilityDeadline = [DateTime]::UtcNow.AddSeconds(360)
        $pickerStabilityComplete = $false
        $pickerStabilityFailure = $false
        while ([DateTime]::UtcNow -lt $pickerStabilityDeadline) {
            if (Test-Path $runtimeLogPath) {
                $stabilityTail = @(
                    Get-Content -LiteralPath $runtimeLogPath -Tail 300 -ErrorAction SilentlyContinue
                )
                if ($stabilityTail -match "bench\.picker_stability failed") {
                    $pickerStabilityFailure = $true
                    break
                }
                if ($stabilityTail -match (
                    "bench\.picker_stability complete channel=$([regex]::Escape($pickerSweepChannel)) stable=true"
                )) {
                    $pickerStabilityComplete = $true
                    break
                }
            }
            Start-Sleep -Milliseconds 100
        }
        if ($pickerStabilityFailure) {
            throw "The $pickerSweepChannel stationary picker-stability gate failed before screenshot capture."
        }
        if (-not $pickerStabilityComplete) {
            throw "The $pickerSweepChannel stationary picker-stability gate did not complete within 360 seconds."
        }

        $pickerWindow = Find-ProcessWindowByTitlePrefix `
            -ProcessId $echoProcess.Id `
            -TitlePrefix "Emotes - $pickerSweepChannel"
        if ($pickerWindow -eq [IntPtr]::Zero) {
            throw "The emote-picker sweep window closed before its stable screenshot could be captured."
        }
        Save-WindowScreenshot -Path $pickerScreenshotPath -WindowHandle $pickerWindow
    }
} catch {
    $originalFailure = $_

    if ($null -ne $popupMonitorOriginalBounds) {
        try {
            Restore-UiFixtureWindowBounds `
                -WindowHandle $mainRootWindowHandle `
                -Bounds $popupMonitorOriginalBounds `
                -TargetName "popup monitor main window failure cleanup"
            $popupMonitorOriginalBounds = $null
        } catch {
            Write-Warning "Could not restore Echo's bounds after popup monitor failure: $($_.Exception.Message)"
        }
    }

    # A failure before the normal Wait-Job/close path must not strand either
    # harness state or the exact Echo process launched for this run.
    try {
        Stop-Job -Job $samplerJob -ErrorAction SilentlyContinue
        Receive-Job -Job $samplerJob -ErrorAction SilentlyContinue | Out-Null
        Remove-Job -Job $samplerJob -ErrorAction SilentlyContinue
    } catch {
        Write-Warning "Could not fully clean up sampler job $($samplerJob.Id): $($_.Exception.Message)"
    }

    try {
        $echoProcess.Refresh()
        if (-not $echoProcess.HasExited) {
            [void]$echoProcess.CloseMainWindow()
            if (-not $echoProcess.WaitForExit(15000)) {
                $remainingWindowsClosed = Close-VisibleProcessWindows -ProcessId $echoProcess.Id
                if ($remainingWindowsClosed -gt 0) {
                    [void]$echoProcess.WaitForExit(15000)
                }
            }
            $echoProcess.Refresh()
            if (-not $echoProcess.HasExited) {
                Write-Warning "Echo process $($echoProcess.Id) did not accept graceful close during failure cleanup and was left running."
            }
        }
    } catch {
        Write-Warning "Could not gracefully close Echo process $($echoProcess.Id): $($_.Exception.Message)"
    }

    throw $originalFailure
}

$completedJob = Wait-Job -Job $samplerJob -Timeout ($DurationSeconds + 45)
if ($null -eq $completedJob) {
    Stop-Job -Job $samplerJob
    throw "Process sampler did not finish within the expected duration."
}
if ($samplerJob.State -ne "Completed") {
    $jobFailure = ($samplerJob.ChildJobs[0].JobStateInfo.Reason | Out-String).Trim()
    throw "Process sampler ended in state '$($samplerJob.State)': $jobFailure"
}
$sampleOutput = (Receive-Job -Job $samplerJob | Out-String).Trim()
Remove-Job -Job $samplerJob

$echoProcess.Refresh()
if ($echoProcess.HasExited) {
    throw "Echo exited before the benchmark harness requested its normal window close."
}
if (-not $echoProcess.CloseMainWindow()) {
    throw "Echo rejected the benchmark harness's graceful close request; process $($echoProcess.Id) was left running."
}
if (-not $echoProcess.WaitForExit(15000)) {
    $remainingWindowsClosed = Close-VisibleProcessWindows -ProcessId $echoProcess.Id
    if ($remainingWindowsClosed -eq 0 -or -not $echoProcess.WaitForExit(15000)) {
        throw "Echo did not exit after the main-window close and fallback close of $remainingWindowsClosed remaining window(s); process $($echoProcess.Id) was left running."
    }
}
$exitCode = $echoProcess.ExitCode
if ($null -ne $exitCode -and $exitCode -ne 0) {
    throw "Echo exited with code $exitCode after the live UI suite."
}

if (-not (Test-Path $rawPath)) {
    throw "Process sampler did not create '$rawPath'. Output: $sampleOutput"
}

$summarizerPath = Join-Path $scriptRoot "summarize-long-session-samples.ps1"
& $summarizerPath -SamplePath $rawPath -App echo -RunDir $runDir -MinimumDurationMilliseconds (($DurationSeconds - 5) * 1000) | Out-Null

$profileOutput = (& $resolvedCheckerPath $runtimeLogPath 2>&1 | Out-String).TrimEnd()
[System.IO.File]::WriteAllText($profileCheckPath, "$profileOutput`r`n")

$runtimeLines = @(Get-Content $runtimeLogPath)
[System.IO.File]::WriteAllLines(
    $gpuSamplesPath,
    @($runtimeLines | Select-String -SimpleMatch 'gpu_ms="' | ForEach-Object { $_.Line })
)
[System.IO.File]::WriteAllLines(
    $providerCachePath,
    @(
        $runtimeLines |
            Select-String -Pattern "emotes\.7tv\.channel\.(network|cache_hit|wait_inflight)" |
            ForEach-Object { $_.Line }
    )
)
[System.IO.File]::WriteAllLines(
    $slowWarningsPath,
    @(
        $runtimeLines |
            Select-String -Pattern "Slow emote pipeline stage|Slow cache preload stage|Rate limited by EventSub API" |
            ForEach-Object { $_.Line }
    )
)
[System.IO.File]::WriteAllLines(
    $connectionEvidencePath,
    @(
        $runtimeLines |
            Select-String -Pattern "USERSTATE|ROOMSTATE|7TV EventAPI WebSocket|7TV EventAPI HELLO|7TV EventAPI ACK" |
            ForEach-Object { $_.Line }
    )
)

$automationEvents = @(
    $runtimeLines |
        Select-String -Pattern "bench\.live_ui_smoke step=|bench\.picker_sweep request" |
        ForEach-Object { $_.Line }
)
[System.IO.File]::WriteAllLines($automationPath, $automationEvents)

$uiFixtureEvents = @(
    $runtimeLines |
        Select-String -SimpleMatch "bench.ui_fixture" |
        ForEach-Object { $_.Line }
)
if ($UiFixtureInteractions) {
    [System.IO.File]::WriteAllLines($uiFixturePath, $uiFixtureEvents)
}

$linkPreviewEvents = @(
    $runtimeLines |
        Select-String -SimpleMatch "bench.link_preview_matrix" |
        ForEach-Object { $_.Line }
)
if ($LinkPreviewMatrix) {
    [System.IO.File]::WriteAllLines($linkPreviewEventsPath, $linkPreviewEvents)
    if ($linkPreviewEvents -match "bench\.link_preview_matrix failed(?:\s|$)" -or $linkPreviewEvents -match "bench\.link_preview_matrix.*(?:^|\s)passed=false(?:\s|$)") {
        throw "Link-preview matrix emitted a failure marker. See '$linkPreviewEventsPath'."
    }

    $initialRows = @($linkPreviewEvents | Where-Object { $_ -match "bench\.link_preview_matrix cell .* stage=initial .*source=local_replay" })
    $shownRows = @($linkPreviewEvents | Where-Object { $_ -match "bench\.link_preview_matrix cell display=manual .* stage=shown .*exact_occurrence=true" })
    $hiddenRows = @($linkPreviewEvents | Where-Object { $_ -match "bench\.link_preview_matrix cell display=manual .* stage=hidden .*exact_occurrence=true" })
    if ($initialRows.Count -ne 12 -or $shownRows.Count -ne 4 -or $hiddenRows.Count -ne 4) {
        throw "Link-preview matrix did not record the expected 12 initial cells and four Manual Show/Hide transitions. See '$linkPreviewEventsPath'."
    }
    foreach ($thumbnail in @("none", "small", "medium", "large")) {
        $noneRow = @($initialRows | Where-Object { $_ -match "display=none thumbnail=$thumbnail .*card_count=0 .*action_present=false .*physical_fetches=0 .*active_flights=0 .*deferred_flights=0 .*passed=true" })
        $alwaysRow = @($initialRows | Where-Object { $_ -match "display=always thumbnail=$thumbnail .*card_count=1 .*action_present=true .*physical_fetches=0 .*active_flights=0 .*deferred_flights=0 .*passed=true" })
        $manualInitial = @($initialRows | Where-Object { $_ -match "display=manual thumbnail=$thumbnail .*card_count=0 .*action_present=true .*physical_fetches=0 .*active_flights=0 .*deferred_flights=0 .*passed=true" })
        $manualShown = @($shownRows | Where-Object { $_ -match "thumbnail=$thumbnail .*card_count=1 .*exact_occurrence=true .*physical_fetches=0 .*active_flights=0 .*deferred_flights=0 .*passed=true" })
        $manualHidden = @($hiddenRows | Where-Object { $_ -match "thumbnail=$thumbnail .*card_count=0 .*exact_occurrence=true .*physical_fetches=0 .*active_flights=0 .*deferred_flights=0 .*passed=true" })
        if ($noneRow.Count -ne 1 -or $alwaysRow.Count -ne 1 -or $manualInitial.Count -ne 1 -or $manualShown.Count -ne 1 -or $manualHidden.Count -ne 1) {
            throw "Link-preview matrix did not satisfy the policy contract for thumbnail '$thumbnail'. See '$linkPreviewEventsPath'."
        }
    }
    if (-not ($linkPreviewEvents -match "bench\.link_preview_matrix complete cells=12 source=local_replay outbound_user_action_attempts=0 send=false passed=true")) {
        throw "Link-preview matrix did not prove the final no-send contract. See '$linkPreviewEventsPath'."
    }
    if (-not (Test-Path $linkPreviewReceiptPath)) {
        throw "Link-preview matrix did not produce its isolated live-channel receipt at '$linkPreviewReceiptPath'."
    }
    $linkPreviewReceipt = Get-Content -Raw -LiteralPath $linkPreviewReceiptPath | ConvertFrom-Json
    if (-not [bool]$linkPreviewReceipt.accepted -or -not [bool]$linkPreviewReceipt.ready -or [bool]$linkPreviewReceipt.send_attempted -or [Int64]$linkPreviewReceipt.outbound_user_action_attempts -ne 0 -or [string]$linkPreviewReceipt.normalized_channel -ne $LinkPreviewMatrixChannel) {
        throw "Link-preview matrix live receipt did not prove a ready #$LinkPreviewMatrixChannel no-send connection. See '$linkPreviewReceiptPath'."
    }
    $summary = [ordered]@{
        source = "local_replay"
        live_channel = $LinkPreviewMatrixChannel
        live_receipt = $linkPreviewReceiptPath
        cells = 12
        manual_show_hide_transitions = 4
        screenshots = @((Get-ChildItem -LiteralPath $runDir -Filter "link-preview-*.png" | ForEach-Object { $_.Name } | Sort-Object))
        outbound_user_action_attempts = [Int64]$linkPreviewReceipt.outbound_user_action_attempts
        send = $false
        accepted = $true
        note = "The deterministic matrix row is locally replayed; any independently received #$LinkPreviewMatrixChannel links are outside this acceptance fixture."
    }
    [System.IO.File]::WriteAllText($linkPreviewSummaryPath, ($summary | ConvertTo-Json -Depth 5), [System.Text.UTF8Encoding]::new($false))
}

$pickerSweepEvents = @(
    $runtimeLines |
        Select-String -SimpleMatch "bench.picker_sweep" |
        ForEach-Object { $_.Line }
)
[System.IO.File]::WriteAllLines($pickerSweepPath, $pickerSweepEvents)

$pickerStabilityEvents = @(
    $runtimeLines |
        Select-String -SimpleMatch "bench.picker_stability" |
        ForEach-Object { $_.Line }
)
[System.IO.File]::WriteAllLines($pickerStabilityPath, $pickerStabilityEvents)

if ($UiFixtureInteractions) {
    if (
        $uiFixtureEvents -match "bench\.ui_fixture failed(?:\s|$)" -or
        $uiFixtureEvents -match "bench\.ui_fixture.*(?:^|\s)passed=false(?:\s|$)" -or
        $uiFixtureEvents -match "bench\.ui_fixture .*?(?:^|\s)sent=true(?:\s|$)"
    ) {
        throw "UI fixture automation emitted a failure marker. See '$uiFixturePath'."
    }

    $requiredUiFixtureMarkers = @(
        @{ Description = "fixture seed"; Pattern = "bench\.ui_fixture fixture_seeded tabs=8 splits=2 offline_channel=echo-offline-fixture live_channel=echo-live-fixture roomstate_labels=5" },
        @{ Description = "header and tab semantic snapshot"; Pattern = "bench\.ui_fixture semantic_snapshot offline_status=OFFLINE offline_title=true offline_category=true live_status=LIVE live_title=true live_category=true live_viewers=12345 roomstate_labels=5 selected_tabs=1 highlighted_tabs=2 new_message_tabs=1 unread_fallback_tabs=1 live_tabs=2 long_title_tabs=1" },
        @{ Description = "header and tabs visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=header_tabs(?:\s|$)" },
        @{ Description = "wide channel popup semantic state"; Pattern = "bench\.ui_fixture state=channel_popup checkpoint=wide channel=echo-offline-fixture width=\d+(?:\.\d+)? expanded=false header_height=48 status=OFFLINE title=true category=true roomstate_labels=5 passed=true" },
        @{ Description = "wide channel popup visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=channel_popup_wide title=Echo_-_#echo-offline-fixture passed=true(?:\s|$)" },
        @{ Description = "narrow channel popup semantic state"; Pattern = "bench\.ui_fixture state=channel_popup checkpoint=narrow channel=echo-offline-fixture width=\d+(?:\.\d+)? expanded=true header_height=68 status=OFFLINE title=true category=true roomstate_labels=5 overlap_safe=true passed=true" },
        @{ Description = "narrow channel popup visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=channel_popup_narrow title=Echo_-_#echo-offline-fixture passed=true(?:\s|$)" },
        @{ Description = "channel popup close state"; Pattern = "bench\.ui_fixture state=channel_popup open=false passed=true(?:\s|$)" },
        @{ Description = "completion interaction cleanup"; Pattern = "bench\.ui_fixture surface=completion stage=complete .*sent=false .*passed=true(?:\s|$)" },
        @{ Description = "default settings surface"; Pattern = "bench\.ui_fixture surface=settings stage=open open=true search_focused=true query_empty=true advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true(?:\s|$)" },
        @{ Description = "default settings visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=settings_default passed=true(?:\s|$)" },
        @{ Description = "up-to-date settings update state"; Pattern = "bench\.ui_fixture surface=settings section=updates destination=General state=up_to_date installed=\S+ latest=\S+ persisted_unchanged=true passed=true(?:\s|$)" },
        @{ Description = "up-to-date settings update visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=settings_updates_up_to_date passed=true(?:\s|$)" },
        @{ Description = "available settings update state"; Pattern = "bench\.ui_fixture surface=settings section=updates destination=General state=available installed=\S+ available=\S+ persisted_unchanged=true passed=true(?:\s|$)" },
        @{ Description = "available settings update visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=settings_updates_available passed=true(?:\s|$)" },
        @{ Description = "ordinary settings search"; Pattern = "bench\.ui_fixture surface=settings stage=query kind=ordinary query=emote selected=Chat result_count=\d+ no_results=false advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true(?:\s|$)" },
        @{ Description = "ordinary settings search visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=settings_search_ordinary passed=true(?:\s|$)" },
        @{ Description = "developer settings hidden search"; Pattern = "bench\.ui_fixture surface=settings stage=query kind=developer_hidden query=fps result_count=0 no_results=true advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true(?:\s|$)" },
        @{ Description = "developer settings hidden visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=settings_search_developer_hidden passed=true(?:\s|$)" },
        @{ Description = "explicit advanced settings reveal"; Pattern = "bench\.ui_fixture surface=settings stage=reveal query=fps selected=Advanced page=advanced anchor=diagnostics result_count=\d+ no_results=false advanced_revealed=true developer_visible=true persisted_unchanged=true passed=true(?:\s|$)" },
        @{ Description = "advanced settings reveal visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=settings_advanced_revealed passed=true(?:\s|$)" },
        @{ Description = "empty settings query safety"; Pattern = "bench\.ui_fixture surface=settings stage=empty query_empty=true result_count=\d+ no_results=false advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true(?:\s|$)" },
        @{ Description = "settings interaction cleanup"; Pattern = "bench\.ui_fixture surface=settings stage=complete open=false query_empty=true advanced_revealed=false persisted_unchanged=true passed=true(?:\s|$)" },
        @{ Description = "tab hover target"; Pattern = "bench\.ui_fixture visual_ready checkpoint=tab_hover target_idx=4 x=-?\d+(?:\.\d+)? y=-?\d+(?:\.\d+)? width=\d+(?:\.\d+)? height=\d+(?:\.\d+)?" },
        @{ Description = "tab hover confirmation"; Pattern = "bench\.ui_fixture interaction=tab_hover target_idx=4 hovered_idx=4 close_visible=true" },
        @{ Description = "tab dropdown target"; Pattern = "bench\.ui_fixture visual_ready checkpoint=dropdown_button x=-?\d+(?:\.\d+)? y=-?\d+(?:\.\d+)? width=\d+(?:\.\d+)? height=\d+(?:\.\d+)? overflow_count=\d+" },
        @{ Description = "tab dropdown confirmation"; Pattern = "bench\.ui_fixture interaction=dropdown_click open=true overflow_count=\d+" },
        @{ Description = "open tab dropdown visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=dropdown_open(?:\s|$)" },
        @{ Description = "multiline tab visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=multiline row_count=\d+ allocated_rows=\d+ tab_bar_height=\d+(?:\.\d+)?" },
        @{ Description = "Activity fixture seed"; Pattern = "bench\.ui_fixture surface=activity stage=seeded mention_present=true whisper_present=true whisper_unread=1 event_present=true" },
        @{ Description = "Activity All state"; Pattern = "bench\.ui_fixture surface=activity stage=section section=All open=true render_delta=\d+" },
        @{ Description = "Activity Mentions state"; Pattern = "bench\.ui_fixture surface=activity stage=section section=Mentions open=true render_delta=\d+" },
        @{ Description = "Activity Whispers state"; Pattern = "bench\.ui_fixture surface=activity stage=section section=Whispers open=true render_delta=\d+" },
        @{ Description = "Activity Events state"; Pattern = "bench\.ui_fixture surface=activity stage=section section=Events open=true render_delta=\d+" },
        @{ Description = "Activity All visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=activity_all passed=true(?:\s|$)" },
        @{ Description = "Activity Mentions visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=activity_mentions passed=true(?:\s|$)" },
        @{ Description = "Activity Whispers visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=activity_whispers passed=true(?:\s|$)" },
        @{ Description = "Activity Events visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=activity_events passed=true(?:\s|$)" },
        @{ Description = "Activity mention dismiss action"; Pattern = "bench\.ui_fixture surface=activity stage=action action=dismiss_mention applied=true mention_present=false" },
        @{ Description = "Activity whisper read action"; Pattern = "bench\.ui_fixture surface=activity stage=action action=mark_whisper_read applied=true whisper_present=true whisper_unread=0" },
        @{ Description = "Activity event dismiss action"; Pattern = "bench\.ui_fixture surface=activity stage=action action=dismiss_event applied=true event_present=false" },
        @{ Description = "Activity completion"; Pattern = "bench\.ui_fixture surface=activity stage=complete open=false mention_present=false whisper_present=false whisper_unread=0 event_present=false" },
        @{ Description = "final transient cleanup"; Pattern = "bench\.ui_fixture teardown path=complete settings_closed=true overflow_closed=true tab_hover_cleared=true activity_closed=true(?:\s|$)" },
        @{ Description = "final texture-cache sweep"; Pattern = "bench\.ui_fixture surface=texture_cache stage=final_sweep before=\d+ removed=\d+ remaining=\d+ live=\d+ passed=true(?:\s|$)" },
        @{ Description = "whisper transport contract"; Pattern = "bench\.ui_fixture surface=whisper_transport endpoint=https://api\.twitch\.tv/helix/whispers scope=user:manage:whispers scope_requested=true request_exact=true login_validation=true api_error_cases=11 errors_mapped=true sanitizer_bounded=true passed=true" },
        @{ Description = "final visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=final(?:\s|$)" },
        @{ Description = "UI fixture completion"; Pattern = "bench\.ui_fixture complete passed=true" }
    )
    if ($UiFixtureSettingsOnly) {
        $requiredUiFixtureMarkers = @(
            @{ Description = "default settings surface"; Pattern = "bench\.ui_fixture surface=settings stage=open open=true search_focused=true query_empty=true advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true(?:\s|$)" },
            @{ Description = "default settings visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=settings_default passed=true(?:\s|$)" },
            @{ Description = "up-to-date settings update state"; Pattern = "bench\.ui_fixture surface=settings section=updates destination=General state=up_to_date installed=\S+ latest=\S+ persisted_unchanged=true passed=true(?:\s|$)" },
            @{ Description = "up-to-date settings update visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=settings_updates_up_to_date passed=true(?:\s|$)" },
            @{ Description = "available settings update state"; Pattern = "bench\.ui_fixture surface=settings section=updates destination=General state=available installed=\S+ available=\S+ persisted_unchanged=true passed=true(?:\s|$)" },
            @{ Description = "available settings update visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=settings_updates_available passed=true(?:\s|$)" },
            @{ Description = "ordinary settings search"; Pattern = "bench\.ui_fixture surface=settings stage=query kind=ordinary query=emote selected=Chat result_count=\d+ no_results=false advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true(?:\s|$)" },
            @{ Description = "ordinary settings search visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=settings_search_ordinary passed=true(?:\s|$)" },
            @{ Description = "developer settings hidden search"; Pattern = "bench\.ui_fixture surface=settings stage=query kind=developer_hidden query=fps result_count=0 no_results=true advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true(?:\s|$)" },
            @{ Description = "developer settings hidden visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=settings_search_developer_hidden passed=true(?:\s|$)" },
            @{ Description = "explicit advanced settings reveal"; Pattern = "bench\.ui_fixture surface=settings stage=reveal query=fps selected=Advanced page=advanced anchor=diagnostics result_count=\d+ no_results=false advanced_revealed=true developer_visible=true persisted_unchanged=true passed=true(?:\s|$)" },
            @{ Description = "advanced settings reveal visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=settings_advanced_revealed passed=true(?:\s|$)" },
            @{ Description = "empty settings query safety"; Pattern = "bench\.ui_fixture surface=settings stage=empty query_empty=true result_count=\d+ no_results=false advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true(?:\s|$)" },
            @{ Description = "settings interaction cleanup"; Pattern = "bench\.ui_fixture surface=settings stage=complete open=false query_empty=true advanced_revealed=false persisted_unchanged=true passed=true(?:\s|$)" },
            @{ Description = "final texture-cache sweep"; Pattern = "bench\.ui_fixture surface=texture_cache stage=final_sweep before=\d+ removed=\d+ remaining=\d+ live=\d+ passed=true(?:\s|$)" },
            @{ Description = "settings-only acceptance completion"; Pattern = "bench\.ui_fixture surface=settings stage=acceptance_complete scope=settings_only persisted_unchanged=true fixture_clean=true passed=true(?:\s|$)" }
        )
    }
    if ($FeedbackUiCheckpoints) {
        $requiredUiFixtureMarkers += @(
            @{ Description = "feedback settings search focus"; Pattern = "bench\.ui_fixture surface=settings stage=open open=true search_focused=true query_empty=true advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true(?:\s|$)" },
            @{ Description = "channel chooser autofocus"; Pattern = "bench\.ui_fixture surface=channel_chooser stage=open open=true input_focused=true query_empty=true placeholder_present=true sent=false passed=true(?:\s|$)" },
            @{ Description = "shared Input mixed-direction selection"; Pattern = "bench\.ui_fixture surface=shared_input stage=bidi_pointer_selection input=production_shared_input native_pointer_drag=true text_len=11 cursor=\d+ cursor_affinity=(?<bidi_end_affinity>leading|trailing) target_selection_start=(?<bidi_start>\d+) target_selection_end=(?<bidi_end>\d+) target_selection_start_affinity=(?:leading|trailing) target_selection_end_affinity=\k<bidi_end_affinity> selection_start=\k<bidi_start> selection_end=\k<bidi_end> selection_span_count=[2-9]\d* selection_span_geometry=-?\d+(?:\.\d+)?:\d+(?:\.\d+)?(?:,-?\d+(?:\.\d+)?:\d+(?:\.\d+)?)+ sent=false passed=true(?:\s|$)" },
            @{ Description = "shared Input mixed-direction visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=feedback_shared_input_bidi_selected passed=true(?:\s|$)" },
            @{ Description = "channel chooser placeholder stability"; Pattern = "bench\.ui_fixture surface=channel_chooser stage=stable open=true input_focused=true query_empty=true placeholder_unchanged=true sent=false passed=true(?:\s|$)" },
            @{ Description = "channel chooser assignment releases focus"; Pattern = "bench\.ui_fixture surface=channel_chooser stage=assigned channel=echo-offline-fixture open=false placeholder_focus=false placeholder_query=false draft_preserved=true sent=false passed=true(?:\s|$)" },
            @{ Description = "channel chooser visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=feedback_channel_chooser passed=true(?:\s|$)" },
            @{ Description = "channel chooser cleanup"; Pattern = "bench\.ui_fixture surface=channel_chooser stage=complete open=false sent=false passed=true(?:\s|$)" },
            @{ Description = "multiline composer geometry"; Pattern = "bench\.ui_fixture surface=composer stage=multiline prepared_in_process=true native_text_input=false line_count=\d+ composer_height=\d+(?:\.\d+)? text_height=\d+(?:\.\d+)? action_height=\d+(?:\.\d+)? overlap_safe=true clipped=false sent=false passed=true(?:\s|$)" },
            @{ Description = "multiline composer visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=feedback_composer_multiline passed=true(?:\s|$)" },
            @{ Description = "multiline composer selection target"; Pattern = "bench\.ui_fixture visual_ready checkpoint=feedback_composer_selection_target x=-?\d+(?:\.\d+)? y=-?\d+(?:\.\d+)? width=\d+(?:\.\d+)? height=\d+(?:\.\d+)? text_len=\d+ selection_len=0 native_mouse=true sent=false passed=true(?:\s|$)" },
            @{ Description = "multiline composer native triple-click selection"; Pattern = "bench\.ui_fixture surface=composer stage=triple_click_select_all native_mouse=true text_len=\d+ selection_start=0 selection_end=\d+ selection_visible=true sent=false passed=true(?:\s|$)" },
            @{ Description = "multiline composer selected visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=feedback_composer_selected_all passed=true(?:\s|$)" },
            @{ Description = "multiline composer reverse-drag target"; Pattern = "bench\.ui_fixture visual_ready checkpoint=feedback_composer_drag_target x=-?\d+(?:\.\d+)? y=-?\d+(?:\.\d+)? width=\d+(?:\.\d+)? height=\d+(?:\.\d+)? text_len=\d+ selection_len=0 native_mouse=true sent=false passed=true(?:\s|$)" },
            @{ Description = "multiline composer native reverse drag"; Pattern = "bench\.ui_fixture surface=composer stage=reverse_drag native_mouse=true text_len=\d+ selection_start=\d+ selection_end=\d+ cursor=\d+ reversed=true selection_visible=true sent=false passed=true(?:\s|$)" },
            @{ Description = "multiline composer reverse-drag visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=feedback_composer_reverse_drag passed=true(?:\s|$)" },
            @{ Description = "multiline composer selection cleanup"; Pattern = "bench\.ui_fixture surface=composer stage=selection_cleanup selection_cleared=true sent=false passed=true(?:\s|$)" },
            @{ Description = "narrow main-window readiness"; Pattern = "bench\.ui_fixture surface=main_window stage=narrow_ready target_width=520 sent=false passed=true(?:\s|$)" },
            @{ Description = "narrow main-window layout"; Pattern = "bench\.ui_fixture surface=main_window stage=narrow width=\d+(?:\.\d+)? layout_safe=true header_overlap=false composer_overlap=false sent=false passed=true(?:\s|$)" },
            @{ Description = "narrow main-window visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=feedback_main_window_narrow passed=true(?:\s|$)" },
            @{ Description = "main-window geometry restoration"; Pattern = "bench\.ui_fixture surface=main_window stage=restored sent=false passed=true(?:\s|$)" },
            @{ Description = "sound slider target"; Pattern = "bench\.ui_fixture visual_ready checkpoint=feedback_sound_slider_target x=-?\d+(?:\.\d+)? y=-?\d+(?:\.\d+)? width=\d+(?:\.\d+)? height=\d+(?:\.\d+)? value_before=\d+(?:\.\d+)? enabled=true persistence_suppressed=true sent=false passed=true(?:\s|$)" },
            @{ Description = "sound slider hover feedback"; Pattern = "bench\.ui_fixture surface=settings stage=sound_slider_hover hovered=true feedback_visible=true value=\d+(?:\.\d+)? persistence_suppressed=true sent=false passed=true(?:\s|$)" },
            @{ Description = "sound slider hover visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=feedback_sound_slider_hover passed=true(?:\s|$)" },
            @{ Description = "sound slider click feedback"; Pattern = "bench\.ui_fixture surface=settings stage=sound_slider_click clicked=true value_after=\d+(?:\.\d+)? value_changed=true feedback_visible=true persistence_suppressed=true sent=false passed=true(?:\s|$)" },
            @{ Description = "sound slider click visual checkpoint"; Pattern = "bench\.ui_fixture visual_ready checkpoint=feedback_sound_slider_click passed=true(?:\s|$)" },
            @{ Description = "sound slider restoration"; Pattern = "bench\.ui_fixture surface=settings stage=sound_slider_complete value_restored=true persisted_unchanged=true sent=false passed=true(?:\s|$)" }
        )
    }
    foreach ($requiredMarker in $requiredUiFixtureMarkers) {
        if (-not ($uiFixtureEvents -match $requiredMarker.Pattern)) {
            throw "Missing required UI fixture marker '$($requiredMarker.Description)'. See '$uiFixturePath'."
        }
    }

    if (-not $UiFixtureSettingsOnly) {
    foreach ($completionTarget in @("main", "thread")) {
        foreach ($completionMode in @("emote", "mention")) {
            $requiredCompletionMarkers = @(
                @{
                    Description = "$completionTarget $completionMode completion suggestions"
                    Pattern = "bench\.ui_fixture surface=completion target=$completionTarget mode=$completionMode stage=suggestions .*focused=true .*passed=true(?:\s|$)"
                },
                @{
                    Description = "$completionTarget $completionMode suggestion acceptance"
                    Pattern = "bench\.ui_fixture surface=completion target=$completionTarget mode=$completionMode stage=suggestion_accept key=native_tab .*sent=false .*passed=true(?:\s|$)"
                }
            )
            foreach ($completionStep in 1..4) {
                $requiredCompletionMarkers += @(
                    @{
                        Description = "$completionTarget $completionMode inline Tab step $completionStep readiness"
                        Pattern = "bench\.ui_fixture surface=completion target=$completionTarget mode=$completionMode stage=inline_ready step=$completionStep .*key=native_tab .*focused=true .*passed=true(?:\s|$)"
                    },
                    @{
                        Description = "$completionTarget $completionMode inline Tab step $completionStep result"
                        Pattern = "bench\.ui_fixture surface=completion target=$completionTarget mode=$completionMode stage=inline_cycle step=$completionStep key=native_tab .*sent=false .*passed=true(?:\s|$)"
                    }
                )
            }

            foreach ($requiredCompletionMarker in $requiredCompletionMarkers) {
                if (-not ($uiFixtureEvents -match $requiredCompletionMarker.Pattern)) {
                    throw "Missing required UI fixture marker '$($requiredCompletionMarker.Description)'. See '$uiFixturePath'."
                }
            }
        }
    }

    $dropdownLine = @(
        $uiFixtureEvents |
            Where-Object { $_ -match "bench\.ui_fixture interaction=dropdown_click open=true" }
    ) | Select-Object -Last 1
    $dropdownOverflowCount = Get-UiFixtureMarkerNumber -Line $dropdownLine -Field "overflow_count"
    if ($dropdownOverflowCount -lt 1) {
        throw "UI fixture dropdown interaction reported no overflow tabs. See '$uiFixturePath'."
    }

    $multilineLine = @(
        $uiFixtureEvents |
            Where-Object { $_ -match "bench\.ui_fixture visual_ready checkpoint=multiline" }
    ) | Select-Object -Last 1
    $multilineRowCount = Get-UiFixtureMarkerNumber -Line $multilineLine -Field "row_count"
    if ($multilineRowCount -lt 2) {
        throw "UI fixture multiline checkpoint rendered fewer than two rows. See '$uiFixturePath'."
    }
    $multilineAllocatedRows = Get-UiFixtureMarkerNumber -Line $multilineLine -Field "allocated_rows"
    if ($multilineAllocatedRows -lt $multilineRowCount) {
        throw "UI fixture multiline checkpoint allocated fewer rows than it rendered. See '$uiFixturePath'."
    }
    $multilineTabBarHeight = Get-UiFixtureMarkerNumber -Line $multilineLine -Field "tab_bar_height"
    if ($multilineTabBarHeight -lt ($multilineRowCount * 36)) {
        throw "UI fixture multiline checkpoint clipped one or more rendered rows. See '$uiFixturePath'."
    }

    $widePopupLine = @(
        $uiFixtureEvents |
            Where-Object {
                $_ -match "bench\.ui_fixture state=channel_popup checkpoint=wide"
            }
    ) | Select-Object -Last 1
    $widePopupWidth = Get-UiFixtureMarkerNumber -Line $widePopupLine -Field "width"
    if ($widePopupWidth -lt 680) {
        throw "UI fixture wide channel-popup state was narrower than 680 px. See '$uiFixturePath'."
    }

    $narrowPopupLine = @(
        $uiFixtureEvents |
            Where-Object {
                $_ -match "bench\.ui_fixture state=channel_popup checkpoint=narrow"
            }
    ) | Select-Object -Last 1
    $narrowPopupWidth = Get-UiFixtureMarkerNumber -Line $narrowPopupLine -Field "width"
    if ($narrowPopupWidth -gt 380 -or $narrowPopupWidth -ge $widePopupWidth) {
        throw "UI fixture narrow channel-popup state did not prove a width at or below 380 px. See '$uiFixturePath'."
    }

    foreach ($section in @("All", "Mentions", "Whispers", "Events")) {
        $sectionLine = @(
            $uiFixtureEvents |
                Where-Object {
                    $_ -match "bench\.ui_fixture surface=activity stage=section section=$section open=true"
                }
        ) | Select-Object -Last 1
        $renderDelta = Get-UiFixtureMarkerNumber -Line $sectionLine -Field "render_delta"
        if ($renderDelta -lt 1) {
            throw "UI fixture Activity $section checkpoint did not observe a render. See '$uiFixturePath'."
        }
    }
    }

    if ($FeedbackUiCheckpoints) {
        $composerLine = @(
            $uiFixtureEvents |
                Where-Object {
                    $_ -match "bench\.ui_fixture surface=composer stage=multiline"
                }
        ) | Select-Object -Last 1
        $composerLineCount = Get-UiFixtureMarkerNumber -Line $composerLine -Field "line_count"
        $composerHeight = Get-UiFixtureMarkerNumber -Line $composerLine -Field "composer_height"
        $composerTextHeight = Get-UiFixtureMarkerNumber -Line $composerLine -Field "text_height"
        $composerActionHeight = Get-UiFixtureMarkerNumber -Line $composerLine -Field "action_height"
        if (
            $composerLineCount -lt 2 -or
            $composerTextHeight -le 0 -or
            $composerActionHeight -le 0 -or
            ($composerHeight + 0.5) -lt [Math]::Max($composerTextHeight, $composerActionHeight)
        ) {
            throw "Feedback UI multiline composer geometry is invalid. See '$uiFixturePath'."
        }

        $composerSelectionLine = @(
            $uiFixtureEvents |
                Where-Object {
                    $_ -match "bench\.ui_fixture surface=composer stage=triple_click_select_all"
                }
        ) | Select-Object -Last 1
        $composerSelectionTextLength = Get-UiFixtureMarkerNumber `
            -Line $composerSelectionLine `
            -Field "text_len"
        $composerSelectionStart = Get-UiFixtureMarkerNumber `
            -Line $composerSelectionLine `
            -Field "selection_start"
        $composerSelectionEnd = Get-UiFixtureMarkerNumber `
            -Line $composerSelectionLine `
            -Field "selection_end"
        if (
            $composerSelectionTextLength -le 0 -or
            $composerSelectionStart -ne 0 -or
            $composerSelectionEnd -ne $composerSelectionTextLength
        ) {
            throw "Feedback UI native triple-click did not select the full composer draft. See '$uiFixturePath'."
        }

        $composerDragLine = @(
            $uiFixtureEvents |
                Where-Object { $_ -match "bench\.ui_fixture surface=composer stage=reverse_drag" }
        ) | Select-Object -Last 1
        $composerDragTextLength = Get-UiFixtureMarkerNumber -Line $composerDragLine -Field "text_len"
        $composerDragStart = Get-UiFixtureMarkerNumber -Line $composerDragLine -Field "selection_start"
        $composerDragEnd = Get-UiFixtureMarkerNumber -Line $composerDragLine -Field "selection_end"
        $composerDragCursor = Get-UiFixtureMarkerNumber -Line $composerDragLine -Field "cursor"
        if (
            $composerDragTextLength -le 0 -or
            $composerDragStart -lt 0 -or
            $composerDragStart -ge $composerDragEnd -or
            $composerDragEnd -ge $composerDragTextLength -or
            $composerDragCursor -ne $composerDragStart
        ) {
            throw "Feedback UI native reverse drag did not produce a bounded reversed selection. See '$uiFixturePath'."
        }

        $narrowMainLine = @(
            $uiFixtureEvents |
                Where-Object {
                    $_ -match "bench\.ui_fixture surface=main_window stage=narrow width="
                }
        ) | Select-Object -Last 1
        $narrowMainWidth = Get-UiFixtureMarkerNumber -Line $narrowMainLine -Field "width"
        if ($narrowMainWidth -lt 500 -or $narrowMainWidth -gt 560) {
            throw "Feedback UI narrow main-window marker reported width $narrowMainWidth outside the 500-560 px acceptance band. See '$uiFixturePath'."
        }

        $soundSliderTargetLine = @(
            $uiFixtureEvents |
                Where-Object {
                    $_ -match "bench\.ui_fixture visual_ready checkpoint=feedback_sound_slider_target"
                }
        ) | Select-Object -Last 1
        $soundSliderClickLine = @(
            $uiFixtureEvents |
                Where-Object {
                    $_ -match "bench\.ui_fixture surface=settings stage=sound_slider_click"
                }
        ) | Select-Object -Last 1
        $soundValueBefore = Get-UiFixtureMarkerNumber -Line $soundSliderTargetLine -Field "value_before"
        $soundValueAfter = Get-UiFixtureMarkerNumber -Line $soundSliderClickLine -Field "value_after"
        if ($soundValueAfter -eq $soundValueBefore) {
            throw "Feedback UI sound-slider click did not change the visible value before restoration. See '$uiFixturePath'."
        }
    }

    $requiredUiFixtureScreenshots = if ($UiFixtureSettingsOnly) {
        @(
            $uiFixtureSettingsDefaultScreenshotPath,
            $uiFixtureSettingsUpdatesUpToDateScreenshotPath,
            $uiFixtureSettingsUpdatesAvailableScreenshotPath,
            $uiFixtureSettingsOrdinarySearchScreenshotPath,
            $uiFixtureSettingsDeveloperHiddenScreenshotPath,
            $uiFixtureSettingsAdvancedRevealScreenshotPath
        )
    } else {
        @(
        $uiFixtureHeaderTabsScreenshotPath,
        $uiFixtureTabHoverScreenshotPath,
        $uiFixtureTabDropdownScreenshotPath,
        $uiFixtureTabMultilineScreenshotPath,
        $uiFixtureActivityAllScreenshotPath,
        $uiFixtureActivityMentionsScreenshotPath,
        $uiFixtureActivityWhispersScreenshotPath,
        $uiFixtureActivityEventsScreenshotPath,
        $uiFixtureChannelPopupWideScreenshotPath,
        $uiFixtureChannelPopupNarrowScreenshotPath,
        $uiFixtureCompletionMainEmoteScreenshotPath,
        $uiFixtureCompletionMainMentionScreenshotPath,
        $uiFixtureCompletionMainCycleScreenshotPath,
        $uiFixtureCompletionWatchingEmoteScreenshotPath,
        $uiFixtureCompletionWatchingCycleScreenshotPath,
        $uiFixtureCompletionThreadEmoteScreenshotPath,
        $uiFixtureCompletionThreadMentionScreenshotPath,
        $uiFixtureCompletionThreadCycleScreenshotPath,
        $uiFixtureSettingsDefaultScreenshotPath,
        $uiFixtureSettingsUpdatesUpToDateScreenshotPath,
        $uiFixtureSettingsUpdatesAvailableScreenshotPath,
        $uiFixtureSettingsOrdinarySearchScreenshotPath,
        $uiFixtureSettingsDeveloperHiddenScreenshotPath,
        $uiFixtureSettingsAdvancedRevealScreenshotPath,
        $uiFixtureFinalScreenshotPath
        )
    }
    if ($FeedbackUiCheckpoints) {
        $requiredUiFixtureScreenshots += @(
            $feedbackChannelChooserScreenshotPath,
            $feedbackSharedInputBidiScreenshotPath,
            $feedbackComposerMultilineScreenshotPath,
            $feedbackComposerSelectedScreenshotPath,
            $feedbackComposerDragScreenshotPath,
            $feedbackNarrowMainScreenshotPath,
            $feedbackSoundSliderHoverScreenshotPath,
            $feedbackSoundSliderClickScreenshotPath
        )
    }
    foreach ($requiredScreenshot in $requiredUiFixtureScreenshots) {
        if (-not (Test-Path $requiredScreenshot)) {
            throw "UI fixture screenshot was not saved to '$requiredScreenshot'."
        }
        if ((Get-Item -LiteralPath $requiredScreenshot).Length -lt 1) {
            throw "UI fixture screenshot '$requiredScreenshot' is empty."
        }
    }
}

if ($pickerSweepEnabled) {
    if ($pickerSweepEvents -match "bench\.picker_sweep failed") {
        throw "The $pickerSweepChannel emote-picker sweep emitted a failure marker. See '$pickerSweepPath'."
    }

    $requiredPickerMarkers = @(
        "bench\.picker_sweep start channel=$([regex]::Escape($pickerSweepChannel)) requested_tab=Channel animation_mode=always native_animated=false",
        "bench\.picker_sweep maximized channel=$([regex]::Escape($pickerSweepChannel)) maximized=true",
        "bench\.picker_sweep forward_complete channel=$([regex]::Escape($pickerSweepChannel)) coverage_complete=true",
        "bench\.picker_sweep reverse_complete channel=$([regex]::Escape($pickerSweepChannel)) coverage_complete=true",
        "bench\.picker_sweep complete channel=$([regex]::Escape($pickerSweepChannel)) coverage_complete=true maximized=true"
    )
    foreach ($marker in $requiredPickerMarkers) {
        if (-not ($pickerSweepEvents -match $marker)) {
            throw "Missing required emote-picker sweep marker '$marker'. See '$pickerSweepPath'."
        }
    }

    $sweepCompleteLine = @(
        $pickerSweepEvents |
            Where-Object {
                $_ -match "bench\.picker_sweep complete channel=$([regex]::Escape($pickerSweepChannel)) coverage_complete=true maximized=true"
            }
    ) | Select-Object -Last 1
    $catalogSizeMatch = [regex]::Match($sweepCompleteLine, "(?:^|\s)emotes=(\d+)(?:\s|$)")
    if (-not $catalogSizeMatch.Success) {
        throw "Picker sweep completion is missing its catalog size. See '$pickerSweepPath'."
    }
    $observedCatalogSize = [int]$catalogSizeMatch.Groups[1].Value
    if ($observedCatalogSize -lt $MinimumEmotePickerCatalogSize) {
        throw "Picker sweep observed only $observedCatalogSize emotes; expected at least $MinimumEmotePickerCatalogSize for the large-catalog gate. See '$pickerSweepPath'."
    }

    if ($pickerStabilityEvents -match "bench\.picker_stability failed") {
        throw "The $pickerSweepChannel stationary picker-stability window emitted a failure marker. See '$pickerStabilityPath'."
    }

    $stabilityStartPattern =
        "bench\.picker_stability start channel=$([regex]::Escape($pickerSweepChannel)) stationary=true animation_mode=always native_animated=false"
    if (-not ($pickerStabilityEvents -match $stabilityStartPattern)) {
        throw "Missing the stationary picker-stability start marker. See '$pickerStabilityPath'."
    }

    $stabilityCompletePattern =
        "bench\.picker_stability complete channel=$([regex]::Escape($pickerSweepChannel)) stable=true"
    $stabilityCompleteLine = @(
        $pickerStabilityEvents |
            Where-Object { $_ -match $stabilityCompletePattern }
    ) | Select-Object -Last 1
    if ([string]::IsNullOrWhiteSpace($stabilityCompleteLine)) {
        throw "Missing the successful stationary picker-stability completion marker. See '$pickerStabilityPath'."
    }
    if ($stabilityCompleteLine -notmatch "stationary=true animation_mode=always native_animated=false animation_timeout=false") {
        throw "Picker-stability validation did not prove bounded Always-mode animation playback. See '$pickerStabilityPath'."
    }

    $stabilityFields = @{}
    foreach ($field in @(
        "expected_visible_cells",
        "expected_visible_animated_cells",
        "observed_unique_cells",
        "rendered_unique_cells",
        "final_placeholder_cells",
        "range_observations",
        "visible_range_changes",
        "rendered_to_placeholder_transitions",
        "post_rendered_placeholder_observations",
        "cell_observations",
        "metadata_ready_animated_cells",
        "advanced_animated_cells",
        "animated_frame_observations",
        "animated_frame_transitions",
        "animated_source_switches",
        "minimum_animated_cell_transitions",
        "maximum_animated_cell_stall_ms",
        "continuity_elapsed_ms"
    )) {
        $match = [regex]::Match(
            $stabilityCompleteLine,
            "(?:^|\s)$([regex]::Escape($field))=(\d+)(?:\s|$)"
        )
        if (-not $match.Success) {
            throw "Picker-stability marker is missing numeric field '$field'. See '$pickerStabilityPath'."
        }
        $stabilityFields[$field] = [Int64]$match.Groups[1].Value
    }

    if ($stabilityFields.expected_visible_cells -lt 1) {
        throw "Picker-stability validation observed no expected visible cells. See '$pickerStabilityPath'."
    }
    if ($stabilityFields.expected_visible_animated_cells -lt 1) {
        throw "Picker-stability validation observed no advertised animated cells. See '$pickerStabilityPath'."
    }
    if ($stabilityFields.observed_unique_cells -ne $stabilityFields.expected_visible_cells) {
        throw "Picker-stability validation did not observe every stationary cell. See '$pickerStabilityPath'."
    }
    if ($stabilityFields.rendered_unique_cells -ne $stabilityFields.expected_visible_cells) {
        throw "Picker-stability validation never rendered every stationary cell. See '$pickerStabilityPath'."
    }
    if ($stabilityFields.final_placeholder_cells -ne 0) {
        throw "Picker-stability validation ended with placeholder cells. See '$pickerStabilityPath'."
    }
    if ($stabilityFields.range_observations -lt 4) {
        throw "Picker-stability validation sampled fewer than four stationary render passes. See '$pickerStabilityPath'."
    }
    if ($stabilityFields.visible_range_changes -ne 0) {
        throw "Picker-stability validation moved the viewport during its stationary window. See '$pickerStabilityPath'."
    }
    if ($stabilityFields.rendered_to_placeholder_transitions -ne 0) {
        throw "Picker-stability validation detected rendered-to-placeholder churn. See '$pickerStabilityPath'."
    }
    if ($stabilityFields.post_rendered_placeholder_observations -ne 0) {
        throw "Picker-stability validation observed placeholders after cells had rendered. See '$pickerStabilityPath'."
    }
    if ($stabilityFields.cell_observations -lt $stabilityFields.expected_visible_cells) {
        throw "Picker-stability validation recorded too few cell observations. See '$pickerStabilityPath'."
    }
    if ($stabilityFields.metadata_ready_animated_cells -ne $stabilityFields.expected_visible_animated_cells) {
        throw "Picker-stability validation did not decode every visible advertised animation. See '$pickerStabilityPath'."
    }
    if ($stabilityFields.advanced_animated_cells -ne $stabilityFields.expected_visible_animated_cells) {
        throw "Picker-stability validation left at least one visible advertised animation frozen. See '$pickerStabilityPath'."
    }
    if ($stabilityFields.animated_frame_observations -lt (2 * $stabilityFields.expected_visible_animated_cells)) {
        throw "Picker-stability validation observed fewer than two displayed frames per advertised animation. See '$pickerStabilityPath'."
    }
    if ($stabilityFields.animated_frame_transitions -lt $stabilityFields.expected_visible_animated_cells) {
        throw "Picker-stability validation recorded too few actual displayed-frame transitions. See '$pickerStabilityPath'."
    }
    if ($stabilityFields.animated_source_switches -ne 0) {
        throw "Picker-stability validation changed animation sources during the post-readiness continuity window. See '$pickerStabilityPath'."
    }
    if ($stabilityFields.minimum_animated_cell_transitions -lt 2) {
        throw "Picker-stability validation did not prove repeated progress for every advertised animation. See '$pickerStabilityPath'."
    }
    if ($stabilityFields.maximum_animated_cell_stall_ms -gt 10000) {
        throw "Picker-stability validation observed an animated cell stalled for more than ten seconds. See '$pickerStabilityPath'."
    }
    if ($stabilityFields.continuity_elapsed_ms -lt 15000) {
        throw "Picker-stability validation did not retain a fifteen-second post-readiness continuity window. See '$pickerStabilityPath'."
    }

    if (-not (Test-Path $pickerScreenshotPath)) {
        throw "The emote-picker sweep screenshot was not saved to '$pickerScreenshotPath'."
    }
}

Write-Output $runDir
