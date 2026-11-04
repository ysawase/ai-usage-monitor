# Isolated GUI smoke. Only execute on an ephemeral interactive Windows VM.
# No global input, no real user profile or credentials, no Store installation changes.
param([Parameter(Mandatory=$true)][string]$ExePath,
      [Parameter(Mandatory=$true)][string]$ExpectedSha256,
      [Parameter(Mandatory=$true)][string]$EvidencePath,
      [switch]$RunInIsolatedGuest)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
if (-not $RunInIsolatedGuest -or $env:GITHUB_ACTIONS -ne 'true') {
    throw 'Refusing non-CI or non-opt-in run.'
}
$cs=Get-CimInstance Win32_ComputerSystem
$vm=[string]$cs.Model -match '(?i)virtual machine|vmware|virtualbox|kvm|qemu|parallels|virtual platform'
if (-not $vm -or -not [Environment]::UserInteractive -or
    [Diagnostics.Process]::GetCurrentProcess().SessionId -eq 0 -or
    [Security.Principal.WindowsIdentity]::GetCurrent().Name -eq 'NT AUTHORITY\SYSTEM') {
    throw 'Not a supported interactive isolated Windows VM.'
}
$exe=[IO.Path]::GetFullPath($ExePath)
if (-not [IO.File]::Exists($exe) -or $exe -match '(?i)\\WindowsApps\\') { throw 'Invalid QA EXE.' }
if ($ExpectedSha256 -notmatch '^[a-fA-F0-9]{64}$') { throw 'Invalid expected hash.' }
$sha=(Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash
if ($sha -ne $ExpectedSha256.ToUpperInvariant()) { throw 'QA EXE hash mismatch.' }
if (@(Get-CimInstance Win32_Process -Filter "Name='claude-code-usage-monitor.exe'").Count -ne 0) {
    throw 'Another monitor instance exists; refusing to interfere.'
}
if (Test-Path -LiteralPath $EvidencePath) { throw 'Evidence file already exists.' }
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class AumQaWin32 {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left,Top,Right,Bottom; }
    private delegate bool EnumProc(IntPtr h, IntPtr arg);
    [DllImport("user32.dll",SetLastError=true)]
    private static extern bool EnumWindows(EnumProc cb,IntPtr arg);
    [DllImport("user32.dll",SetLastError=true)]
    private static extern uint GetWindowThreadProcessId(IntPtr h,out uint pid);
    [DllImport("user32.dll")]
    private static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)]
    private static extern int GetClassNameW(IntPtr h,StringBuilder s,int cap);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)]
    private static extern int GetWindowTextW(IntPtr h,StringBuilder s,int cap);
    [DllImport("user32.dll",SetLastError=true)]
    public static extern bool GetClientRect(IntPtr h,out RECT r);
    [DllImport("user32.dll",SetLastError=true)]
    public static extern uint GetDpiForWindow(IntPtr h);
    [DllImport("user32.dll",SetLastError=true)]
    public static extern bool PostMessageW(IntPtr h,uint m,IntPtr w,IntPtr l);
    [DllImport("user32.dll",SetLastError=true)]
    public static extern IntPtr GetWindow(IntPtr h,uint command);
    public static IntPtr Find(int pid,string name) {
        IntPtr result=IntPtr.Zero;
        EnumWindows(delegate(IntPtr h,IntPtr unused) {
            uint ownerPid;GetWindowThreadProcessId(h,out ownerPid);
            if(ownerPid!=(uint)pid||!IsWindowVisible(h))return true;
            var sb=new StringBuilder(128);GetClassNameW(h,sb,sb.Capacity);
            if(sb.ToString()!=name)return true;
            result=h;return false;
        },IntPtr.Zero);
        return result;
    }
    public static string Title(IntPtr h) {
        var sb=new StringBuilder(256);
        GetWindowTextW(h,sb,sb.Capacity);
        return sb.ToString();
    }
}
'@
$runId=[guid]::NewGuid().ToString('N')
$sandbox=Join-Path $env:RUNNER_TEMP ("aum-gui-qa-"+$runId)
$profileDir=Join-Path $sandbox 'Profile'
$roaming=Join-Path $sandbox 'Roaming'
$local=Join-Path $sandbox 'Local'
foreach($d in @($sandbox,$profileDir,$roaming,$local)) {
    [void][IO.Directory]::CreateDirectory($d)
}
$proc=$null
$main=[IntPtr]::Zero
$dialog=[IntPtr]::Zero
$evidence=[ordered]@{
    test='isolated-win32-help-button-smoke'
    utc=[DateTime]::UtcNow.ToString('o')
    status='not_run'
    exe_sha256=$sha
    vm_model=[string]$cs.Model
    session=[Diagnostics.Process]::GetCurrentProcess().SessionId
    dialog_title=$null
    dialog_owner_verified=$false
    env_scrubbed=$false
    process_exited=$false
    note='Synthetic process-targeted Win32 click, not physical input or auth recovery.'
}
try {
    $psi=New-Object Diagnostics.ProcessStartInfo
    $psi.FileName=$exe
    $psi.WorkingDirectory=$sandbox
    $psi.UseShellExecute=$false
    $systemRoot=$env:SystemRoot
    if([string]::IsNullOrWhiteSpace($systemRoot) -or -not (Test-Path $systemRoot)) {throw 'No valid Windows root.'}
    $psi.EnvironmentVariables.Clear()
    $psi.EnvironmentVariables['SystemRoot']=$systemRoot
    $psi.EnvironmentVariables['windir']=$systemRoot
    $psi.EnvironmentVariables['ComSpec']=Join-Path $systemRoot 'System32\cmd.exe'
    $psi.EnvironmentVariables['Path']=(Join-Path $systemRoot 'System32')+';'+$systemRoot
    $psi.EnvironmentVariables['TEMP']=$sandbox
    $psi.EnvironmentVariables['TMP']=$sandbox
    $psi.EnvironmentVariables['APPDATA']=$roaming
    $psi.EnvironmentVariables['LOCALAPPDATA']=$local
    $psi.EnvironmentVariables['USERPROFILE']=$profileDir
    $psi.EnvironmentVariables['HOME']=$profileDir
    $psi.EnvironmentVariables['CODEX_HOME']=Join-Path $profileDir '.codex'
    foreach($k in @('ANTHROPIC_API_KEY','OPENAI_API_KEY','GH_TOKEN','GITHUB_TOKEN','AI_GATEWAY_API_KEY','VERCEL_TOKEN')) {
        if($psi.EnvironmentVariables.ContainsKey($k)){throw 'Credential environment inherited.'}
    }
    $evidence.env_scrubbed=$true
    $proc=[Diagnostics.Process]::Start($psi)
    if($null -eq $proc){throw 'Process did not start.'}
    for($i=0;$i -lt 150;$i++) {
        if($proc.HasExited){throw 'Process exited before presenting its own UI.'}
        $main=[AumQaWin32]::Find($proc.Id,'ClaudeCodeUsageMonitor')
        if($main -ne [IntPtr]::Zero){break}
        Start-Sleep -Milliseconds 100
    }
    if($main -eq [IntPtr]::Zero){throw 'No visible QA monitor window.'}
    $rect=New-Object AumQaWin32+RECT
    if(-not [AumQaWin32]::GetClientRect($main,[ref]$rect)){throw 'Client rect unavailable.'}
    $width=$rect.Right-$rect.Left
    $height=$rect.Bottom-$rect.Top
    $dpi=[AumQaWin32]::GetDpiForWindow($main)
    if($dpi -le 0){$dpi=96}
    # Corresponds to help_button_rect_for_client_width in review commit.
    $sc={param([int]$v) [int][Math]::Round($v*[double]$dpi/96.0,[MidpointRounding]::AwayFromZero)}
    $size=& $sc 14
    $edge=[Math]::Max((& $sc 6),4)
    $right=[Math]::Max(($width-$edge-(& $sc 4)),$size)
    $x=$right-[int][Math]::Ceiling($size/2.0)
    $y=(& $sc 3)+[int][Math]::Floor($size/2.0)
    if($width -lt 200 -or $height -lt 20 -or $x -lt $edge -or $x -ge $width-$edge -or $y -ge $height){throw 'Help point out of bounds.'}
    if([AumQaWin32]::Find($proc.Id,'#32770') -ne [IntPtr]::Zero){throw 'Dialog already existed.'}
    $lp=[IntPtr](($y -shl 16) -bor ($x -band 0xFFFF))
    if(-not [AumQaWin32]::PostMessageW($main,0x0201,[IntPtr]::Zero,$lp)){throw 'Down message rejected.'}
    if(-not [AumQaWin32]::PostMessageW($main,0x0202,[IntPtr]::Zero,$lp)){throw 'Up message rejected.'}
    for($i=0;$i -lt 100;$i++) {
        if($proc.HasExited){throw 'QA process exited before help dialog.'}
        $dialog=[AumQaWin32]::Find($proc.Id,'#32770')
        if($dialog -ne [IntPtr]::Zero){break}
        Start-Sleep -Milliseconds 100
    }
    if($dialog -eq [IntPtr]::Zero){throw 'Help dialog not observed.'}
    $title=[AumQaWin32]::Title($dialog)
    $expectedTitles=@('表示の見方','How to Read the Display','AIサービスの接続診断','AI provider connection diagnosis')
    if($expectedTitles -notcontains $title){throw ('Unexpected dialog title: '+$title)}
    if([AumQaWin32]::GetWindow($dialog,4) -ne $main){throw 'Dialog is not owned by QA main window.'}
    $evidence.dialog_title=$title
    $evidence.dialog_owner_verified=$true
    $evidence.status='pass_help_click_opens_dialog'
} catch {
    $evidence.status='fail'
    $evidence.note=$_.Exception.Message
    throw
} finally {
    if($dialog -ne [IntPtr]::Zero){[void][AumQaWin32]::PostMessageW($dialog,0x0010,[IntPtr]::Zero,[IntPtr]::Zero)}
    if($main -ne [IntPtr]::Zero){[void][AumQaWin32]::PostMessageW($main,0x0010,[IntPtr]::Zero,[IntPtr]::Zero)}
    if($null -ne $proc) {
        try {
            if(-not $proc.HasExited) {
                if(-not $proc.WaitForExit(3000)){$proc.Kill();[void]$proc.WaitForExit(3000)}
            }
            $evidence.process_exited=$proc.HasExited
        } catch {$evidence.process_exited=$false}
        $proc.Dispose()
    }
    if($evidence.status -eq 'pass_help_click_opens_dialog' -and
       (-not $evidence.process_exited -or -not $evidence.env_scrubbed -or -not $evidence.dialog_owner_verified)) {
        $evidence.status='fail'
        $evidence.note='Cleanup or isolation not verified.'
    }
    $evidence|ConvertTo-Json -Depth 5|Set-Content -LiteralPath $EvidencePath -Encoding UTF8
    if($evidence.process_exited -and (Test-Path $sandbox)) {Remove-Item $sandbox -Recurse -Force}
    Write-Output ("GUI_EVIDENCE_STATUS="+$evidence.status)
    Write-Output ("GUI_EVIDENCE_PATH="+$EvidencePath)
}
