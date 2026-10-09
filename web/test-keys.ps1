$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms,System.Drawing
Add-Type -TypeDefinition @'
using System;using System.Runtime.InteropServices;
public static class K {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out R r);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] public struct R { public int l,t,r,b; }
  public const uint WM_KEYDOWN=0x100, WM_KEYUP=0x101;
  public static void Key(IntPtr h,int vk){ PostMessage(h,WM_KEYDOWN,(IntPtr)vk,(IntPtr)1); System.Threading.Thread.Sleep(90); PostMessage(h,WM_KEYUP,(IntPtr)vk,(IntPtr)1); }
}
'@

$env:RUST_BACKTRACE='full'; $env:SION_TEST_AUTOEXIT_SECS='30'; $env:SION_DEBUG_FLIP='1'
$p = Start-Process -FilePath "target\debug\title_menu.exe" -PassThru `
     -RedirectStandardError crash_err.log -RedirectStandardOutput crash_out.log
Start-Sleep -Seconds 3        # let MML compile + first frames render
if (-not (Get-Process -Id $p.Id -ErrorAction SilentlyContinue)) { "process died before keys!"; Get-Content crash_err.log; exit 1 }
$h = $p.MainWindowHandle
"[key] Down x2";     [K]::Key($h,0x28); Start-Sleep -m 350; [K]::Key($h,0x28); Start-Sleep -m 500
"[key] Up";          [K]::Key($h,0x27); Start-Sleep -m 500
"[key] Enter->dialogue"; [K]::Key($h,0x0D); Start-Sleep -m 600
"[key] Enter advance x3"; (1..3) | ForEach-Object { [K]::Key($h,0x0D); Start-Sleep -m 450 }
"[key] Escape back"; [K]::Key($h,0x1B); Start-Sleep -m 450
"[key] Down to Options"; [K]::Key($h,0x28); [K]::Key($h,0x28); Start-Sleep -m 350
"[key] Enter options"; [K]::Key($h,0x0D); Start-Sleep -m 350
"[key] Right x4 slider"; (1..4) | ForEach-Object { [K]::Key($h,0x27); Start-Sleep -m 140 }
"[key] Down slider row"; [K]::Key($h,0x28); Start-Sleep -m 140
"[key] Right x3 sfx"; (1..3) | ForEach-Object { [K]::Key($h,0x27); Start-Sleep -m 140 }
"[key] Escape back";   [K]::Key($h,0x1B); Start-Sleep -m 350
$alive = Get-Process -Id $p.Id -ErrorAction SilentlyContinue
if ($alive) { "SURVIVED interactive session"; $r = New-Object K+R; [void][K]::GetWindowRect($h,[ref]$r);
  $w=$r.r-$r.l;$hh=$r.b-$r.t; $bmp=New-Object System.Drawing.Bitmap($w,$hh)
  $g=[System.Drawing.Graphics]::FromImage($bmp); $g.CopyFromScreen($r.l,$r.t,0,0,(New-Object System.Drawing.Size($w,$hh)))
  $bmp.Save("opencode_final.png",[System.Drawing.Imaging.ImageFormat]::Png); $g.Dispose();$bmp.Dispose()
  Stop-Process -Id $p.Id -Force } else { "DIED during keys"; }
"--- stderr tail ---"; Get-Content crash_err.log -ErrorAction SilentlyContinue | Select-Object -Last 25
