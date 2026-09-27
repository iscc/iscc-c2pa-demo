# Drive and capture the running ISCC C2PA Demo window, for checking the UI by eye.
# Start the app first, e.g. `pnpm tauri dev -- -- <absolute path to a file>`, and stop it with
# `taskkill /IM iscc-c2pa-demo.exe /F` when done. Use -NoFocus while a native dialog is open, or the
# keystrokes go to the main window. Take a screenshot before clicking: coordinates shift with banners.
# Usage: pwsh scripts/shot.ps1 -Out shot.png [-ClickX x -ClickY y] [-Wheel n] [-Keys 'text{ENTER}'] [-Wait ms] [-NoFocus] [-Width w -Height h]
# Coordinates are physical pixels relative to the window's top-left corner; the window is moved to
# -Width x -Height (default 1440x1260, the narrowest layout at 150% display scaling).
param([string]$Out, [int]$ClickX = -1, [int]$ClickY = -1, [string]$Keys = "", [int]$Wheel = 0, [int]$WheelX = 1400, [int]$WheelY = 700, [int]$Wait = 0, [switch]$NoFocus, [int]$Width = 1440, [int]$Height = 1260)
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class W {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool r);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
[W]::SetProcessDPIAware() | Out-Null
$h = (Get-Process iscc-c2pa-demo -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1).MainWindowHandle
if ($h -eq [IntPtr]::Zero) { Write-Error "window not found"; exit 1 }
if (-not $NoFocus) { [W]::MoveWindow($h, 0, 0, $Width, $Height, $true) | Out-Null; [W]::SetForegroundWindow($h) | Out-Null }
Start-Sleep -Milliseconds 300
$r = New-Object W+RECT
[W]::GetWindowRect($h, [ref]$r) | Out-Null
if ($ClickX -ge 0) {
  [W]::SetCursorPos($r.L + $ClickX, $r.T + $ClickY) | Out-Null
  [W]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero); [W]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 400
}
if ($Wheel -ne 0) {
  [W]::SetCursorPos($r.L + $WheelX, $r.T + $WheelY) | Out-Null
  for ($i = 0; $i -lt [Math]::Abs($Wheel); $i++) { [W]::mouse_event(0x0800, 0, 0, [uint32]($(if ($Wheel -gt 0) { 4294967176 } else { 120 })), [UIntPtr]::Zero); Start-Sleep -Milliseconds 40 }
  Start-Sleep -Milliseconds 300
}
if ($Keys -ne "") { [System.Windows.Forms.SendKeys]::SendWait($Keys); Start-Sleep -Milliseconds 600 }
if ($Wait -gt 0) { Start-Sleep -Milliseconds $Wait }
$w = $r.R - $r.L; $hh = $r.B - $r.T
$bmp = New-Object System.Drawing.Bitmap $w, $hh
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
Write-Output "saved $Out ($w x $hh)"
