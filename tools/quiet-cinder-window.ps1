# Optional visible-window capture, for hover states not supported by --smoke-actions.
param(
    [Parameter(Mandatory = $true)][int]$SatinPid,
    [Parameter(Mandatory = $true)][string]$Viewport,
    [switch]$Resize,
    [string]$Png,
    [ValidateRange(0, 30)][int]$DelaySeconds = 4
)

$ErrorActionPreference = 'Stop'
if ([bool]$Resize -eq [bool]$Png) { throw 'Use exactly one of -Resize or -Png.' }
$viewports = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'quiet-cinder-viewports.json') | ConvertFrom-Json
$size = $viewports.PSObject.Properties | Where-Object { $_.Name -ceq $Viewport } | Select-Object -ExpandProperty Value
if (-not $size) { throw "Unknown viewport '$Viewport'. Expected $($viewports.PSObject.Properties.Name -join ', ')." }
$width, $height = $size
if (-not ('QuietCinder.Window' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

namespace QuietCinder {
    public static class Window {
        [StructLayout(LayoutKind.Sequential)]
        private struct Rect { public int Left, Top, Right, Bottom; }
        [StructLayout(LayoutKind.Sequential)]
        private struct Point { public int X, Y; }
        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool GetWindowRect(IntPtr window, out Rect rect);
        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool GetClientRect(IntPtr window, out Rect rect);
        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool ClientToScreen(IntPtr window, ref Point point);
        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool SetWindowPos(IntPtr window, IntPtr behind, int x, int y, int w, int h, uint flags);
        [DllImport("user32.dll")] private static extern uint GetDpiForWindow(IntPtr window);
        [DllImport("user32.dll")] private static extern bool IsZoomed(IntPtr window);
        [DllImport("user32.dll")] private static extern bool IsIconic(IntPtr window);
        [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
        [DllImport("user32.dll")] private static extern bool GetCursorPos(out Point point);
        public static int[] Geometry(IntPtr window) {
            if (window == IntPtr.Zero || IsIconic(window) || IsZoomed(window))
                throw new InvalidOperationException("Satin must have a restored, visible window.");
            if (GetDpiForWindow(window) != 96)
                throw new InvalidOperationException("Satin must be on a 96-DPI (100%) display.");
            Rect client;
            var origin = new Point();
            if (!GetClientRect(window, out client) || !ClientToScreen(window, ref origin))
                throw new Win32Exception();
            return new[] { origin.X, origin.Y, client.Right - client.Left, client.Bottom - client.Top };
        }
        public static void Resize(IntPtr window, int width, int height) {
            int[] current = Geometry(window);
            Rect outer;
            if (!GetWindowRect(window, out outer) ||
                !SetWindowPos(window, IntPtr.Zero, 0, 0, outer.Right - outer.Left - current[2] + width,
                    outer.Bottom - outer.Top - current[3] + height, 0x216))
                throw new Win32Exception();
        }
        public static int[] Cursor(IntPtr window) {
            if (GetForegroundWindow() != window)
                throw new InvalidOperationException("Satin must be foreground and unobscured.");
            var g = Geometry(window);
            Point mouse;
            if (!GetCursorPos(out mouse)) throw new Win32Exception();
            return new[] { mouse.X - g[0], mouse.Y - g[1] };
        }
    }
}
'@
}

$process = Get-Process -Id $SatinPid
$hwnd = $process.MainWindowHandle
if ($Resize) { [QuietCinder.Window]::Resize($hwnd, $width, $height) }
if ($Png -and $DelaySeconds) { Start-Sleep -Seconds $DelaySeconds }
$geometry = [QuietCinder.Window]::Geometry($hwnd)
if ($geometry[2] -ne $width -or $geometry[3] -ne $height) {
    throw "Client is $($geometry[2])x$($geometry[3]), expected ${width}x${height}."
}

$pointer = $null
if ($Png) {
    $pointer = [QuietCinder.Window]::Cursor($hwnd)
    Add-Type -AssemblyName System.Drawing, System.Windows.Forms
    $rect = [Drawing.Rectangle]::new($geometry[0], $geometry[1], $width, $height)
    if (-not @([Windows.Forms.Screen]::AllScreens | Where-Object { $_.Bounds.Contains($rect) }).Count) {
        throw 'The whole Satin client must be inside one usable display.'
    }
    $bitmap = [Drawing.Bitmap]::new($width, $height, [Drawing.Imaging.PixelFormat]::Format24bppRgb)
    try {
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try { $graphics.CopyFromScreen($rect.Location, [Drawing.Point]::Empty, $rect.Size) }
        finally { $graphics.Dispose() }
        # Reject a move, focus change, or pointer movement during acquisition.
        if (([QuietCinder.Window]::Cursor($hwnd) -join ',') -ne ($pointer -join ',') -or
            ([QuietCinder.Window]::Geometry($hwnd) -join ',') -ne ($geometry -join ',')) {
            throw 'Window or pointer moved during capture.'
        }
        $path = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Png)
        $stream = [IO.File]::Open($path, [IO.FileMode]::CreateNew)
        try { $bitmap.Save($stream, [Drawing.Imaging.ImageFormat]::Png) } finally { $stream.Dispose() }
    } finally { $bitmap.Dispose() }
}

[ordered]@{
    pid = $SatinPid; executable = $process.Path; viewport = $Viewport; dpi = 96
    client = $geometry; pointer = $pointer; png = $Png
} | ConvertTo-Json -Compress
