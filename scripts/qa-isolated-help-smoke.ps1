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
using System.Collections.Generic;
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
    [DllImport("user32.dll",SetLastError=true)]
    private static extern bool EnumChildWindows(IntPtr parent,EnumProc callback,IntPtr arg);
    [DllImport("user32.dll",CharSet=CharSet.Unicode,SetLastError=true)]
    private static extern IntPtr SendMessageTimeoutW(IntPtr hwnd,uint msg,IntPtr w,StringBuilder text,uint flags,uint timeout,out IntPtr result);
    public static string DialogText(IntPtr dialog) {
        var parts=new List<string>();
        EnumChildWindows(dialog,delegate(IntPtr child,IntPtr unused) {
            var cls=new StringBuilder(128);
            GetClassNameW(child,cls,cls.Capacity);
            if(cls.ToString()=="Static") {
                var sb=new StringBuilder(8192);
                IntPtr result;
                IntPtr ok=SendMessageTimeoutW(child,0x000D,(IntPtr)sb.Capacity,sb,2,500,out result);
                if(ok!=IntPtr.Zero && sb.Length>0)parts.Add(sb.ToString());
            }
            return true;
        },IntPtr.Zero);
        return String.Join("\n",parts.ToArray());
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
    $psi.EnvironmentVariables['GITHUB_ACTIONS']='true'
    $psi.EnvironmentVariables['AUM_QA_DIAG']='isolated-fixture-v1'
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
    if(-not [AumQaWin32]::PostMessageW($dialog,0x0010,[IntPtr]::Zero,[IntPtr]::Zero)){throw 'Initial help dialog close rejected.'}
    for($i=0;$i -lt 50;$i++) {
        if([AumQaWin32]::Find($proc.Id,'#32770') -eq [IntPtr]::Zero){break}
        Start-Sleep -Milliseconds 100
    }
    if([AumQaWin32]::Find($proc.Id,'#32770') -ne [IntPtr]::Zero){throw 'Initial help dialog did not close.'}
    $dialog=[IntPtr]::Zero
    $cases=@(
        @{id=1; label='visible_session_failure'; mustInclude=@('Claude Code (session):','Codex (weekly):'); mustExclude=@('Claude Code (weekly):')},
        @{id=2; label='weekly_failure'; mustInclude=@('Claude Code (weekly):','Codex (weekly):'); mustExclude=@('Claude Code (session):')},
        @{id=3; label='disabled_codex_excluded'; mustInclude=@('Claude Code (weekly):'); mustExclude=@('Codex (','GitHub Copilot','Vercel AI Gateway')},
        @{id=4; label='hidden_session_failure'; mustInclude=@('Claude Code (weekly):'); mustExclude=@('Claude Code (session):')},
        @{id=5; label='both_fail_weekly_priority'; mustInclude=@('Claude Code (weekly):'); mustExclude=@('Claude Code (session):')}
    )
    $evidence.fixture_cases=@()
    foreach($fixture in $cases) {
        if(-not [AumQaWin32]::PostMessageW($main,0x805A,[IntPtr]([int]$fixture['id']),[IntPtr]::Zero)) {
            throw ('Fixture message rejected: '+$fixture['label'])
        }
        $dialog=[IntPtr]::Zero
        for($i=0;$i -lt 100;$i++) {
            if($proc.HasExited){throw ('Process exited during '+$fixture['label'])}
            $dialog=[AumQaWin32]::Find($proc.Id,'#32770')
            if($dialog -ne [IntPtr]::Zero){break}
            Start-Sleep -Milliseconds 100
        }
        if($dialog -eq [IntPtr]::Zero){throw ('Diagnosis dialog not observed: '+$fixture['label'])}
        $title=[AumQaWin32]::Title($dialog)
        if($title -ne 'AI provider connection diagnosis'){throw ('Unexpected diagnosis title: '+$title)}
        if([AumQaWin32]::GetWindow($dialog,4) -ne $main){throw ('Wrong dialog owner: '+$fixture['label'])}
        $body=[AumQaWin32]::DialogText($dialog)
        if(-not $body.Contains('Latest polling states.')){throw ('No diagnosis body: '+$fixture['label'])}
        foreach($needle in $fixture['mustInclude']){
            if(-not $body.Contains($needle)){throw ('Expected phrase missing: '+$fixture['label']+' '+$needle)}
        }
        foreach($needle in $fixture['mustExclude']){
            if($body.Contains($needle)){throw ('Unexpected phrase: '+$fixture['label']+' '+$needle)}
        }
        $evidence.fixture_cases+=@{ id=$fixture['id']; label=$fixture['label']; status='pass'; title=$title; owner_verified=$true }
        if(-not [AumQaWin32]::PostMessageW($dialog,0x0010,[IntPtr]::Zero,[IntPtr]::Zero)) {
            throw ('Diagnosis close rejected: '+$fixture['label'])
        }
        for($i=0;$i -lt 50;$i++) {
            if([AumQaWin32]::Find($proc.Id,'#32770') -eq [IntPtr]::Zero){break}
            Start-Sleep -Milliseconds 100
        }
        if([AumQaWin32]::Find($proc.Id,'#32770') -ne [IntPtr]::Zero){throw ('Diagnosis did not close: '+$fixture['label'])}
        $dialog=[IntPtr]::Zero
    }
    # This is the genuine Antigravity menu path in the normal product code.
    # Only its own disposable-VM process receives WM_COMMAND; do not install
    # any CLI, open a browser, or use real credentials.
    $ag=[ordered]@{
        setup_dialog='not_run'
        connection_dialog='not_run'
        external_guide_declined=$false
    }
    if(-not [AumQaWin32]::PostMessageW($main,0x0111,[IntPtr]104,[IntPtr]::Zero)){
        throw 'Antigravity setup WM_COMMAND rejected'
    }
    $dialog=[IntPtr]::Zero
    for($i=0;$i -lt 100;$i++){
        if($proc.HasExited){throw 'QA process exited during Antigravity setup'}
        $dialog=[AumQaWin32]::Find($proc.Id,'#32770')
        if($dialog -ne [IntPtr]::Zero){break}
        Start-Sleep -Milliseconds 100
    }
    if($dialog -eq [IntPtr]::Zero){throw 'Antigravity setup dialog missing'}
    if([AumQaWin32]::GetWindow($dialog,4) -ne $main){throw 'Antigravity setup dialog owner mismatch'}
    $agTitle=[AumQaWin32]::Title($dialog)
    $agBody=[AumQaWin32]::DialogText($dialog)
    if($agTitle -ne 'Antigravity Setup' -or
       -not $agBody.Contains('/statusline aum-quota.exe antigravity-statusline-bridge') -or
       -not $agBody.Contains('/statusline delete')){
        throw ('Antigravity setup title or safe commands not found. Title='+$agTitle)
    }
    $ag.setup_dialog='pass'
    if(-not [AumQaWin32]::PostMessageW($dialog,0x0010,[IntPtr]::Zero,[IntPtr]::Zero)){
        throw 'Antigravity setup dialog close rejected'
    }
    for($i=0;$i -lt 100;$i++){
        $dialog=[AumQaWin32]::Find($proc.Id,'#32770')
        if($dialog -ne [IntPtr]::Zero -and [AumQaWin32]::DialogText($dialog).Contains('Open the official Antigravity CLI installation guide')){break}
        Start-Sleep -Milliseconds 100
    }
    if($dialog -eq [IntPtr]::Zero -or
       -not [AumQaWin32]::DialogText($dialog).Contains('Open the official Antigravity CLI installation guide')){
        throw 'Expected isolated-VM Antigravity guide confirmation missing'
    }
    # IDNO=7: specifically reject launching a browser or installing CLI.
    if(-not [AumQaWin32]::PostMessageW($dialog,0x0111,[IntPtr]7,[IntPtr]::Zero)){
        throw 'Failed to decline optional external documentation'
    }
    for($i=0;$i -lt 50;$i++){
        if([AumQaWin32]::Find($proc.Id,'#32770') -eq [IntPtr]::Zero){break}
        Start-Sleep -Milliseconds 100
    }
    if([AumQaWin32]::Find($proc.Id,'#32770') -ne [IntPtr]::Zero){
        throw 'Antigravity guide confirmation failed to close'
    }
    $ag.external_guide_declined=$true
    $dialog=[IntPtr]::Zero
    if(-not [AumQaWin32]::PostMessageW($main,0x0111,[IntPtr]105,[IntPtr]::Zero)){
        throw 'Antigravity connection check WM_COMMAND rejected'
    }
    for($i=0;$i -lt 100;$i++){
        if($proc.HasExited){throw 'QA process exited during Antigravity connection check'}
        $dialog=[AumQaWin32]::Find($proc.Id,'#32770')
        if($dialog -ne [IntPtr]::Zero){break}
        Start-Sleep -Milliseconds 100
    }
    if($dialog -eq [IntPtr]::Zero){throw 'Antigravity connection dialog missing'}
    if([AumQaWin32]::GetWindow($dialog,4) -ne $main){throw 'Antigravity connection dialog owner mismatch'}
    $connTitle=[AumQaWin32]::Title($dialog)
    $connBody=[AumQaWin32]::DialogText($dialog)
    if($connTitle -ne 'Antigravity connection check' -or
       -not $connBody.Contains('No quota data has been received.')){
        throw ('Antigravity connection result not consistent with isolated empty profile: '+$connTitle)
    }
    $ag.connection_dialog='pass'
    if(-not [AumQaWin32]::PostMessageW($dialog,0x0010,[IntPtr]::Zero,[IntPtr]::Zero)){
        throw 'Antigravity connection dialog close rejected'
    }
    for($i=0;$i -lt 50;$i++){
        if([AumQaWin32]::Find($proc.Id,'#32770') -eq [IntPtr]::Zero){break}
        Start-Sleep -Milliseconds 100
    }
    if([AumQaWin32]::Find($proc.Id,'#32770') -ne [IntPtr]::Zero){throw 'Antigravity connection dialog failed to close'}
    $dialog=[IntPtr]::Zero
    $evidence.antigravity=$ag
    $evidence.status='pass_help_diagnostics_antigravity'
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
    if($evidence.status -eq 'pass_help_diagnostics_antigravity' -and
       (-not $evidence.process_exited -or -not $evidence.env_scrubbed -or -not $evidence.dialog_owner_verified)) {
        $evidence.status='fail'
        $evidence.note='Cleanup or isolation not verified.'
    }
    $evidence|ConvertTo-Json -Depth 5|Set-Content -LiteralPath $EvidencePath -Encoding UTF8
    if($evidence.process_exited -and (Test-Path $sandbox)) {Remove-Item $sandbox -Recurse -Force}
    Write-Output ("GUI_EVIDENCE_STATUS="+$evidence.status)
    Write-Output ("GUI_EVIDENCE_PATH="+$EvidencePath)
}
