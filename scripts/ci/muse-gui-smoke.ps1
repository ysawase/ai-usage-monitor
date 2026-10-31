# Windows-only GUI smoke test: synthetic Muse quota, no provider auth or requests.
# Run in an isolated GitHub Actions Windows runner; no real user data is read.
param([string]$AppPath = "target/release/claude-code-usage-monitor.exe")
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
$exe = (Resolve-Path -LiteralPath $AppPath).Path

Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class MuseWin32Smoke {
  public delegate bool EnumProc(IntPtr hwnd, IntPtr param);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr data);
  [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr parent, EnumProc cb, IntPtr data);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassNameW(IntPtr hwnd, StringBuilder text, int size);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowTextW(IntPtr hwnd, StringBuilder text, int size);
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hwnd, uint msg, IntPtr wparam, IntPtr lparam);
  static string ClassOf(IntPtr hwnd) {var b=new StringBuilder(256);GetClassNameW(hwnd,b,b.Capacity);return b.ToString();}
  public static IntPtr Find(int pid, string desiredClass) {
    IntPtr found=IntPtr.Zero;
    EnumWindows((hwnd,param)=>{
      uint p; GetWindowThreadProcessId(hwnd,out p);
      if(p==(uint)pid && ClassOf(hwnd)==desiredClass) { found=hwnd;return false;}
      return true;
    },IntPtr.Zero);
    return found;
  }
  public static string DialogText(IntPtr hwnd) {
    if(hwnd==IntPtr.Zero) return "";
    var text=new StringBuilder();
    EnumChildWindows(hwnd,(child,param)=>{
      var b=new StringBuilder(32768);GetWindowTextW(child,b,b.Capacity);
      if(b.Length>0) text.AppendLine(b.ToString());
      return true;
    },IntPtr.Zero);
    return text.ToString();
  }
}
'@

$originalLocal = $env:LOCALAPPDATA
$sandbox = Join-Path $env:RUNNER_TEMP ("aum-muse-ui-" + [guid]::NewGuid().ToString("N"))
$env:LOCALAPPDATA = $sandbox
$cacheDir = Join-Path $sandbox "Solidaily\AIUsageMonitor"
New-Item -ItemType Directory -Force -Path $cacheDir | Out-Null
$cache = Join-Path $cacheDir "muse-observation-v1.json"
$process = $null

function Observe-Dialog([int]$targetProcessId, [string]$expected, [string]$case) {
  $main = [MuseWin32Smoke]::Find($targetProcessId,"ClaudeCodeUsageMonitor")
  if($main -eq [IntPtr]::Zero){throw "$case : application window missing"}
  if(-not [MuseWin32Smoke]::PostMessageW($main,0x111,[IntPtr]107,[IntPtr]::Zero)){
    throw "$case : WM_COMMAND dispatch failed"
  }
  $dialog=[IntPtr]::Zero
  $text=""
  for($i=0;$i -lt 100;$i++){
    Start-Sleep -Milliseconds 200
    $dialog=[MuseWin32Smoke]::Find($targetProcessId,"#32770")
    $text=[MuseWin32Smoke]::DialogText($dialog)
    if($text.Contains($expected)){break}
  }
  if(-not $text.Contains($expected)){
    throw "$case : expected UI dialog text not found; dialog=$($dialog -ne [IntPtr]::Zero), actual=$text"
  }
  Write-Output "MUSE_GUI_SMOKE PASS $case : matched UI dialog text"
  [MuseWin32Smoke]::PostMessageW($dialog,0x0010,[IntPtr]::Zero,[IntPtr]::Zero) | Out-Null
  for($i=0;$i -lt 30;$i++){
    if([MuseWin32Smoke]::Find($targetProcessId,"#32770") -eq [IntPtr]::Zero){break}
    Start-Sleep -Milliseconds 100
  }
}
try {
  $now=[DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
  $reading=[ordered]@{
    schema_version=1
    source="muse_browser_bridge"
    observed_at_unix=$now
    plan="Muse Synthetic Test"
    weekly_used_percent=43
    weekly_resets_at_unix=($now + 86400)
  }
  ($reading | ConvertTo-Json -Compress) | Set-Content -Encoding utf8NoBOM -LiteralPath $cache
  $process=Start-Process -FilePath $exe -PassThru -WorkingDirectory (Split-Path $exe) -WindowStyle Normal
  $window=[IntPtr]::Zero
  for($i=0;$i -lt 100;$i++){
    Start-Sleep -Milliseconds 200
    $process.Refresh()
    if($process.HasExited) {throw "application exited before GUI initialized ($($process.ExitCode))"}
    $window=[MuseWin32Smoke]::Find($process.Id,"ClaudeCodeUsageMonitor")
    if($window -ne [IntPtr]::Zero){break}
  }
  if($window -eq [IntPtr]::Zero){throw "application did not expose a GUI window (runner desktop may be unavailable)"}
  Write-Output "MUSE_GUI_SMOKE: app native window located"

  Observe-Dialog $process.Id "Muse Synthetic Test" "current-plan"
  Observe-Dialog $process.Id "43%" "current-used-percent"

  $reading.observed_at_unix=($now - 900)
  ($reading | ConvertTo-Json -Compress) | Set-Content -Encoding utf8NoBOM -LiteralPath $cache
  Observe-Dialog $process.Id "stale" "stale-reading"

  $reading.observed_at_unix=$now
  $reading["cookie"]="SYNTHETIC_FORBIDDEN_FIELD"
  ($reading | ConvertTo-Json -Compress) | Set-Content -Encoding utf8NoBOM -LiteralPath $cache
  Observe-Dialog $process.Id "could not be validated" "unknown-field-rejected"

  Remove-Item -LiteralPath $cache -Force
  Observe-Dialog $process.Id "not connected" "disconnected"
  Write-Output "MUSE_GUI_SMOKE PASS all 5 GUI assertions"
}
finally {
  if($null -ne $process){
    try {if(-not $process.HasExited){Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue}}catch{}
    $process.Dispose()
  }
  $env:LOCALAPPDATA=$originalLocal
  Remove-Item -LiteralPath $sandbox -Force -Recurse -ErrorAction SilentlyContinue
}
