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
    public static extern bool GetWindowRect(IntPtr h,out RECT r);
    [DllImport("user32.dll",SetLastError=true)]
    public static extern bool SetWindowPos(IntPtr h,IntPtr after,int x,int y,int width,int height,uint flags);
    [DllImport("user32.dll",CharSet=CharSet.Unicode,SetLastError=true)]
    private static extern IntPtr FindWindowW(string name,string title);
    [DllImport("user32.dll",CharSet=CharSet.Unicode,SetLastError=true)]
    private static extern IntPtr FindWindowExW(IntPtr parent,IntPtr after,string name,string title);
    public static IntPtr FindTrayNotify() {
        IntPtr taskbar=FindWindowW("Shell_TrayWnd",null);
        return taskbar==IntPtr.Zero ? IntPtr.Zero : FindWindowExW(taskbar,IntPtr.Zero,"TrayNotifyWnd",null);
    }
    public static uint OwnerPid(IntPtr h) {
        uint pid;
        GetWindowThreadProcessId(h,out pid);
        return pid;
    }
    [StructLayout(LayoutKind.Sequential)]
    public struct MONITORINFO {
        public int cbSize;
        public RECT rcMonitor;
        public RECT rcWork;
        public uint dwFlags;
    }
    [DllImport("user32.dll")]
    private static extern IntPtr MonitorFromWindow(IntPtr h,uint flags);
    [DllImport("user32.dll",CharSet=CharSet.Unicode,SetLastError=true)]
    private static extern bool GetMonitorInfoW(IntPtr monitor,ref MONITORINFO info);
    public static RECT WorkArea(IntPtr h) {
        var monitor=MonitorFromWindow(h,2);
        if(monitor==IntPtr.Zero)throw new InvalidOperationException("No monitor for own popup");
        var mi=new MONITORINFO();
        mi.cbSize=Marshal.SizeOf(typeof(MONITORINFO));
        if(!GetMonitorInfoW(monitor,ref mi))throw new InvalidOperationException("GetMonitorInfo failed");
        return mi.rcWork;
    }
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

public static class AumOwnWindowWinEventProbe {
    private delegate void WinEventCallback(IntPtr hook, uint eventId, IntPtr hwnd, int idObject, int idChild, uint eventThread, uint eventTime);
    [System.Runtime.InteropServices.StructLayout(System.Runtime.InteropServices.LayoutKind.Sequential)]
    private struct MSG {
        public IntPtr hwnd; public uint message; public UIntPtr wParam; public IntPtr lParam;
        public uint time; public int pointX; public int pointY; public uint lPrivate;
    }
    [System.Runtime.InteropServices.DllImport("user32.dll",EntryPoint="CreateWindowExW",CharSet=System.Runtime.InteropServices.CharSet.Unicode,SetLastError=true)]
    private static extern IntPtr CreateWindowEx(uint exStyle,string className,string title,uint style,int x,int y,int w,int h,IntPtr parent,IntPtr menu,IntPtr instance,IntPtr param);
    [System.Runtime.InteropServices.DllImport("user32.dll",SetLastError=true)]
    private static extern bool DestroyWindow(IntPtr hwnd);
    [System.Runtime.InteropServices.DllImport("user32.dll",SetLastError=true)]
    private static extern uint GetWindowThreadProcessId(IntPtr hwnd,out uint processId);
    [System.Runtime.InteropServices.DllImport("user32.dll",SetLastError=true)]
    private static extern IntPtr SetWinEventHook(uint min,uint max,IntPtr module,WinEventCallback callback,uint process,uint thread,uint flags);
    [System.Runtime.InteropServices.DllImport("user32.dll",SetLastError=true)]
    private static extern bool UnhookWinEvent(IntPtr hook);
    [System.Runtime.InteropServices.DllImport("user32.dll",SetLastError=true)]
    private static extern bool SetWindowPos(IntPtr hwnd,IntPtr insertAfter,int x,int y,int w,int h,uint flags);
    [System.Runtime.InteropServices.DllImport("user32.dll",SetLastError=true)]
    private static extern bool PostMessageW(IntPtr hwnd,uint message,IntPtr wParam,IntPtr lParam);
    [System.Runtime.InteropServices.DllImport("user32.dll",SetLastError=true)]
    private static extern bool PeekMessageW(out MSG msg,IntPtr hwnd,uint min,uint max,uint remove);
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern bool TranslateMessage(ref MSG msg);
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern IntPtr DispatchMessageW(ref MSG msg);
    private static IntPtr source;
    private static IntPtr receiver;
    private static int armed;
    private static int callbacks;
    private static int delivered;
    private static WinEventCallback callbackRoot;
    private static void OnEvent(IntPtr hook,uint eventId,IntPtr hwnd,int idObject,int idChild,uint eventThread,uint eventTime) {
        if(eventId != 0x800B || hwnd != source || idObject != 0 || idChild != 0 ||
           System.Threading.Volatile.Read(ref armed) != 1) return;
        System.Threading.Interlocked.Increment(ref callbacks);
        if(PostMessageW(receiver,0x8005,IntPtr.Zero,IntPtr.Zero))
            System.Threading.Interlocked.Exchange(ref delivered,1);
    }
    public static string Run(IntPtr productWindow) {
        receiver=productWindow;
        source=IntPtr.Zero;
        armed=0; callbacks=0; delivered=0;
        IntPtr hook=IntPtr.Zero;
        try {
            // Standard Windows STATIC window; owned by this temporary QA process.
            // Never move Explorer's taskbar, other apps, or display settings.
            source=CreateWindowEx(0,"STATIC","AUM isolated WinEvent source",0x90000000,80,80,140,40,
                                  IntPtr.Zero,IntPtr.Zero,IntPtr.Zero,IntPtr.Zero);
            if(source==IntPtr.Zero) throw new InvalidOperationException("Cannot create own WinEvent source");
            uint pid;
            uint threadId=GetWindowThreadProcessId(source,out pid);
            if(threadId==0 || pid!=(uint)System.Diagnostics.Process.GetCurrentProcess().Id)
                throw new InvalidOperationException("Own WinEvent source thread identity invalid");
            callbackRoot=OnEvent;
            hook=SetWinEventHook(0x800B,0x800B,IntPtr.Zero,callbackRoot,0,threadId,0);
            if(hook==IntPtr.Zero) throw new InvalidOperationException("Own-window WinEvent hook could not register");
            System.Threading.Thread.Sleep(250);
            System.Threading.Volatile.Write(ref armed,1);
            if(!SetWindowPos(source,IntPtr.Zero,93,91,0,0,0x0015))
                throw new InvalidOperationException("Own-window SetWindowPos failed");
            var deadline=System.Diagnostics.Stopwatch.StartNew();
            while(deadline.ElapsedMilliseconds<5000 && System.Threading.Volatile.Read(ref delivered)==0) {
                MSG message;
                while(PeekMessageW(out message,IntPtr.Zero,0,0,1)) {
                    TranslateMessage(ref message);
                    DispatchMessageW(ref message);
                }
                System.Threading.Thread.Sleep(20);
            }
            if(System.Threading.Volatile.Read(ref callbacks)<1)
                throw new InvalidOperationException("No OS WinEvent callback for own-window location change");
            if(System.Threading.Volatile.Read(ref delivered)!=1)
                throw new InvalidOperationException("Own-window WinEvent did not post reposition message");
            return "pass";
        } finally {
            if(hook!=IntPtr.Zero) UnhookWinEvent(hook);
            if(source!=IntPtr.Zero) DestroyWindow(source);
            callbackRoot=null;
            source=IntPtr.Zero;
        }
    }
}

// OS input automation ONLY in the disposable GitHub Windows VM.
// No real user desktop, no foreground activation, no physical input.
public static class AumQaOsInput {
    [System.Runtime.InteropServices.StructLayout(System.Runtime.InteropServices.LayoutKind.Sequential)]
    private struct POINT { public int X,Y; }
    [System.Runtime.InteropServices.StructLayout(System.Runtime.InteropServices.LayoutKind.Sequential)]
    private struct MOUSEINPUT {
        public int dx,dy; public uint mouseData,dwFlags,time;
        public IntPtr dwExtraInfo;
    }
    [System.Runtime.InteropServices.StructLayout(System.Runtime.InteropServices.LayoutKind.Sequential)]
    private struct INPUT { public uint type; public MOUSEINPUT mi; }
    [System.Runtime.InteropServices.DllImport("user32.dll",SetLastError=true)]
    private static extern bool ClientToScreen(IntPtr h,ref POINT pt);
    [System.Runtime.InteropServices.DllImport("user32.dll",SetLastError=true)]
    private static extern bool GetCursorPos(out POINT pt);
    [System.Runtime.InteropServices.DllImport("user32.dll",SetLastError=true)]
    private static extern bool SetCursorPos(int x,int y);
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern IntPtr WindowFromPoint(POINT pt);
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern IntPtr GetAncestor(IntPtr h,uint flags);
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr h,out uint pid);
    [System.Runtime.InteropServices.DllImport("user32.dll",SetLastError=true)]
    private static extern uint SendInput(uint count,INPUT[] inputs,int size);
    [System.Runtime.InteropServices.DllImport("user32.dll",SetLastError=true)]
    private static extern IntPtr SendMessageW(IntPtr h,uint msg,IntPtr w,IntPtr l);
    public static int QueryCta(IntPtr main,int code) {
        return unchecked((int)SendMessageW(main,0x805B,new IntPtr(code),IntPtr.Zero).ToInt64());
    }
    public static string HoverClickOwnedCta(IntPtr main,int left,int top,int right,int bottom,int expectedPid,int expectedAction,int index=0,int expectedCount=1) {
        POINT origin;
        if(!GetCursorPos(out origin)) throw new InvalidOperationException("No VM cursor origin");
        if(left<=0||top<=0||right-left<8||bottom-top<8)
            throw new InvalidOperationException("Invalid CTA rectangle");
        if(QueryCta(main,1)!=expectedCount||QueryCta(main,5+16*index)!=expectedAction)
            throw new InvalidOperationException("Wrong CTA before input");
        try {
            bool owned=false;
            int mid=(top+bottom)/2;
            int[] dy=new int[]{0,-2,2,-4,4};
            for(int x=left+3;x<right-2&&!owned;x+=3) {
                foreach(int offset in dy) {
                    int y=mid+offset;
                    if(y<=top||y>=bottom) continue;
                    POINT px=new POINT{X=x,Y=y};
                    if(!ClientToScreen(main,ref px)) continue;
                    if(!SetCursorPos(px.X,px.Y)) continue;
                    System.Threading.Thread.Sleep(35);
                    POINT now;
                    if(!GetCursorPos(out now)||now.X!=px.X||now.Y!=px.Y) continue;
                    IntPtr h=WindowFromPoint(now);
                    uint pid=0;
                    if(h!=IntPtr.Zero&&GetWindowThreadProcessId(h,out pid)!=0
                        &&pid==(uint)expectedPid&&GetAncestor(h,2)==main) {
                        owned=true; break;
                    }
                }
            }
            if(!owned) throw new InvalidOperationException("No QA-owned CTA pixel, click suppressed");
            bool hovered=false;
            for(int i=0;i<35;i++) {
                if(QueryCta(main,4+16*index)==1) {hovered=true;break;}
                System.Threading.Thread.Sleep(60);
            }
            if(!hovered) throw new InvalidOperationException("CTA hover not observed");
            if(QueryCta(main,5+16*index)!=expectedAction)
                throw new InvalidOperationException("CTA action changed after hover");
            INPUT down=new INPUT{type=0,mi=new MOUSEINPUT{dwFlags=0x0002}};
            INPUT up=new INPUT{type=0,mi=new MOUSEINPUT{dwFlags=0x0004}};
            int size=System.Runtime.InteropServices.Marshal.SizeOf(typeof(INPUT));
            uint sent=SendInput(2,new INPUT[]{down,up},size);
            if(sent!=2) {
                if(sent==1) SendInput(1,new INPUT[]{up},size);
                throw new InvalidOperationException("CTA SendInput incomplete");
            }
            return "hover_then_os_sendinput";
        } finally { SetCursorPos(origin.X,origin.Y); }
    }
    public static string ClickOwnedHelp(IntPtr main,int x,int y,int expectedPid) {
        POINT origin,pt;
        if(!GetCursorPos(out origin)) throw new InvalidOperationException("No original VM pointer");
        pt=new POINT{X=x,Y=y};
        if(!ClientToScreen(main,ref pt)) throw new InvalidOperationException("Cannot project help position");
        try {
            if(!SetCursorPos(pt.X,pt.Y)) throw new InvalidOperationException("Cannot move VM mouse");
            System.Threading.Thread.Sleep(250);
            POINT actual;
            if(!GetCursorPos(out actual)||actual.X!=pt.X||actual.Y!=pt.Y)
                throw new InvalidOperationException("VM mouse failed to reach requested pixel");
            IntPtr hit=WindowFromPoint(actual);
            uint pid=0;
            if(hit==IntPtr.Zero||GetWindowThreadProcessId(hit,out pid)==0||
               pid!=(uint)expectedPid||GetAncestor(hit,2)!=main)
                throw new InvalidOperationException("Pointer target is not QA-owned application; input suppressed");
            INPUT down=new INPUT{type=0,mi=new MOUSEINPUT{dwFlags=0x0002}};
            INPUT up=new INPUT{type=0,mi=new MOUSEINPUT{dwFlags=0x0004}};
            int size=System.Runtime.InteropServices.Marshal.SizeOf(typeof(INPUT));
            uint sent=SendInput(2,new INPUT[]{down,up},size);
            if(sent!=2) {
                if(sent==1) SendInput(1,new INPUT[]{up},size);
                throw new InvalidOperationException("Mouse press/release was not fully injected");
            }
            return "os_sendinput_owned_mouse";
        } finally { SetCursorPos(origin.X,origin.Y); }
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
    # QA-only attention path: fixture 6 sets visible session failure, but
    # unlike fixtures 1-5 it DOES NOT directly open the diagnosis dialog.
    # Exercise the real production WM_LBUTTONDOWN hit test at the help icon.
    if(-not [AumQaWin32]::PostMessageW($main,0x805A,[IntPtr]6,[IntPtr]::Zero)){
        throw 'Failed to arm help-attention fixture'
    }
    Start-Sleep -Milliseconds 200
    if([AumQaWin32]::Find($proc.Id,'#32770') -ne [IntPtr]::Zero){
        throw 'Help-attention fixture unexpectedly opened dialog without click'
    }
    $helpRect=New-Object AumQaWin32+RECT
    if(-not [AumQaWin32]::GetClientRect($main,[ref]$helpRect)){
        throw 'Help client rect not measurable after fixture'
    }
    $helpWidth=$helpRect.Right-$helpRect.Left
    $helpHeight=$helpRect.Bottom-$helpRect.Top
    $helpDpi=[AumQaWin32]::GetDpiForWindow($main)
    if($helpDpi -le 0){$helpDpi=96}
    $helpScale={param([int]$v) [int][Math]::Round($v*[double]$helpDpi/96.0,[MidpointRounding]::AwayFromZero)}
    $helpSize=& $helpScale 14
    $helpEdge=[Math]::Max((& $helpScale 6),4)
    $helpRight=[Math]::Max(($helpWidth-$helpEdge-(& $helpScale 4)),$helpSize)
    $helpX=$helpRight-[int][Math]::Ceiling($helpSize/2.0)
    $helpY=(& $helpScale 3)+[int][Math]::Floor($helpSize/2.0)
    if($helpX -le 0 -or $helpX -ge $helpWidth -or $helpY -ge $helpHeight){
        throw 'Attention help click outside window'
    }
    $helpLp=[IntPtr](($helpY -shl 16) -bor ($helpX -band 0xFFFF))
    if(-not [AumQaWin32]::PostMessageW($main,0x0201,[IntPtr]::Zero,$helpLp)){
        throw 'Attention help WM_LBUTTONDOWN rejected'
    }
    if(-not [AumQaWin32]::PostMessageW($main,0x0202,[IntPtr]::Zero,$helpLp)){
        throw 'Attention help WM_LBUTTONUP rejected'
    }
    $attentionDialog=[IntPtr]::Zero
    for($i=0;$i -lt 100;$i++){
        if($proc.HasExited){throw 'QA process exited during attention help test'}
        $attentionDialog=[AumQaWin32]::Find($proc.Id,'#32770')
        if($attentionDialog -ne [IntPtr]::Zero){break}
        Start-Sleep -Milliseconds 100
    }
    if($attentionDialog -eq [IntPtr]::Zero){
        throw 'Help icon failed to open connection diagnosis under visible session failure'
    }
    if([AumQaWin32]::Title($attentionDialog) -ne 'AI provider connection diagnosis' -or
       [AumQaWin32]::GetWindow($attentionDialog,4) -ne $main){
        throw 'Help-attention diagnosis title or owner mismatch'
    }
    $attentionText=[AumQaWin32]::DialogText($attentionDialog)
    if(-not $attentionText.Contains('Claude Code (session):') -or
       $attentionText.Contains('Claude Code (weekly):')){
        throw 'Help-attention diagnosis did not label visible session failure'
    }
    $evidence.help_attention_click=[ordered]@{
        status='pass'
        fixture='visible_session_failure_without_direct_dialog'
        path='real_product_WM_LBUTTONDOWN_help_hit_test'
        diagnosis='Claude Code (session)'
        physical_mouse_tested=$false
    }
    if(-not [AumQaWin32]::PostMessageW($attentionDialog,0x0010,[IntPtr]::Zero,[IntPtr]::Zero)){
        throw 'Help-attention dialog close message rejected'
    }
    for($i=0;$i -lt 50;$i++){
        if([AumQaWin32]::Find($proc.Id,'#32770') -eq [IntPtr]::Zero){break}
        Start-Sleep -Milliseconds 100
    }
    if([AumQaWin32]::Find($proc.Id,'#32770') -ne [IntPtr]::Zero){
        throw 'Help-attention dialog failed to close'
    }
    $dialog=[IntPtr]::Zero
    # Stronger QA input evidence: Windows SendInput, not PostMessage. The
    # input helper checks the exact HWND and PID under the pointer first.
    # The product uses WS_EX_NOACTIVATE; do not force foreground activation.
    if(-not [AumQaWin32]::PostMessageW($main,0x805A,[IntPtr]6,[IntPtr]::Zero)){
        throw 'OS-mouse fixture rearm failed'
    }
    Start-Sleep -Milliseconds 250
    if([AumQaWin32]::Find($proc.Id,'#32770') -ne [IntPtr]::Zero){
        throw 'OS-mouse fixture opened dialog before mouse input'
    }
    $osMode=[AumQaOsInput]::ClickOwnedHelp($main,$helpX,$helpY,$proc.Id)
    $osDialog=[IntPtr]::Zero
    for($i=0;$i -lt 100;$i++){
        if($proc.HasExited){throw 'QA app exited after OS mouse input'}
        $osDialog=[AumQaWin32]::Find($proc.Id,'#32770')
        if($osDialog -ne [IntPtr]::Zero){break}
        Start-Sleep -Milliseconds 100
    }
    if($osDialog -eq [IntPtr]::Zero){throw 'OS mouse input did not open a diagnosis dialog'}
    $osTitle=[AumQaWin32]::Title($osDialog)
    $osBody=[AumQaWin32]::DialogText($osDialog)
    if($osTitle -ne 'AI provider connection diagnosis' -or
       [AumQaWin32]::GetWindow($osDialog,4) -ne $main -or
       -not $osBody.Contains('Claude Code (session):') -or
       $osBody.Contains('Claude Code (weekly):')){
        throw 'OS input diagnosis not owned or did not use session-failure state'
    }
    $evidence.os_mouse_click=[ordered]@{
        status='pass'
        mode=$osMode
        own_window_target_verified=$true
        no_foreground_activation_requested=$true
        session_diagnosis_verified=$true
        human_physical_mouse_tested=$false
        real_user_desktop_tested=$false
    }
    if(-not [AumQaWin32]::PostMessageW($osDialog,0x0010,[IntPtr]::Zero,[IntPtr]::Zero)){
        throw 'OS input dialog close rejected'
    }
    for($i=0;$i -lt 50;$i++){
        if([AumQaWin32]::Find($proc.Id,'#32770') -eq [IntPtr]::Zero){break}
        Start-Sleep -Milliseconds 100
    }
    if([AumQaWin32]::Find($proc.Id,'#32770') -ne [IntPtr]::Zero){
        throw 'OS input dialog failed to close'
    }
    $dialog=[IntPtr]::Zero
    # QA fixtures 7/8: verify hover + OS mouse input on the product CTA.
    # AuthAction::launch is intercepted in the ephemeral runner checkout,
    # before any real console, browser, CLI, or OAuth action is possible.
    $evidence.auth_cta_cases=@()
    $authCases=@(
        @{ id=7; action=1; name='claude'; expected='qa_claude_login_dispatch_blocked' },
        @{ id=8; action=2; name='codex'; expected='qa_codex_login_dispatch_blocked' }
    )
    foreach($case in $authCases) {
        if(-not [AumQaWin32]::PostMessageW($main,0x805A,[IntPtr]([int]$case.id),[IntPtr]::Zero)){
            throw ('Failed to arm auth CTA state: '+$case.name)
        }
        $ctaReady=$false
        for($i=0;$i -lt 70;$i++){
            if($proc.HasExited){throw ('QA product exited before CTA: '+$case.name)}
            $count=[AumQaOsInput]::QueryCta($main,1)
            $action=[AumQaOsInput]::QueryCta($main,5)
            if($count -eq 1 -and $action -eq $case.action){$ctaReady=$true;break}
            Start-Sleep -Milliseconds 70
        }
        if(-not $ctaReady){throw ('Auth CTA target not painted: '+$case.name)}
        if([AumQaWin32]::Find($proc.Id,'#32770') -ne [IntPtr]::Zero){
            throw ('Fixture opened dialog before CTA click: '+$case.name)
        }
        $left=[AumQaOsInput]::QueryCta($main,6)
        $top=[AumQaOsInput]::QueryCta($main,7)
        $right=[AumQaOsInput]::QueryCta($main,8)
        $bottom=[AumQaOsInput]::QueryCta($main,9)
        if([AumQaOsInput]::QueryCta($main,4) -ne 0){
            throw ('Unexpected initial CTA hover: '+$case.name)
        }
        $mode=[AumQaOsInput]::HoverClickOwnedCta(
            $main,$left,$top,$right,$bottom,$proc.Id,[int]$case.action)
        $authDialog=[IntPtr]::Zero
        for($i=0;$i -lt 100;$i++){
            if($proc.HasExited){throw ('QA app exited after CTA click: '+$case.name)}
            $authDialog=[AumQaWin32]::Find($proc.Id,'#32770')
            if($authDialog -ne [IntPtr]::Zero){break}
            Start-Sleep -Milliseconds 100
        }
        if($authDialog -eq [IntPtr]::Zero){
            throw ('Safe auth CTA intercept dialog missing: '+$case.name)
        }
        $title=[AumQaWin32]::Title($authDialog)
        $body=[AumQaWin32]::DialogText($authDialog)
        if($title -ne 'QA auth dispatch intercepted' -or
           [AumQaWin32]::GetWindow($authDialog,4) -ne $main -or
           -not $body.Contains([string]$case.expected)){
            throw ('Unexpected auth CTA dispatch: '+$case.name)
        }
        $evidence.auth_cta_cases+=@{
            name=$case.name
            status='pass'
            simulated_state='weekly_authentication_expired'
            action=$case.expected
            pointer_hover='pass'
            input=$mode
            owned_product_dialog=$true
            external_auth_launch_intercepted=$true
            physical_human_mouse_tested=$false
            natural_auth_failure_tested=$false
        }
        if(-not [AumQaWin32]::PostMessageW($authDialog,0x0010,[IntPtr]::Zero,[IntPtr]::Zero)){
            throw ('Auth CTA dialog close rejected: '+$case.name)
        }
        for($i=0;$i -lt 50;$i++){
            if([AumQaWin32]::Find($proc.Id,'#32770') -eq [IntPtr]::Zero){break}
            Start-Sleep -Milliseconds 100
        }
        if([AumQaWin32]::Find($proc.Id,'#32770') -ne [IntPtr]::Zero){
            throw ('Auth CTA intercept dialog remained open: '+$case.name)
        }
        $dialog=[IntPtr]::Zero
    }
    # Mixed-state row-specific dispatch: fixture 9 simultaneously exposes
    # weekly=AuthenticationExpired (ClaudeLogin) and session=FetchFailed
    # (ProviderDiagnosis). Click each independently using OS SendInput.
    # QA-only AuthAction interceptor prevents CLI/OAuth/browser effects.
    $evidence.cell_target_cases=@()
    $evidence.mixed_geometry=@()
    if(-not [AumQaWin32]::PostMessageW($main,0x805A,[IntPtr]9,[IntPtr]::Zero)){
        throw 'Unable to arm dual-window mixed failure fixture'
    }
    $mixedReady=$false
    for($i=0;$i -lt 70;$i++){
        if($proc.HasExited){throw 'Product exited before mixed-state test'}
        if([AumQaOsInput]::QueryCta($main,1) -eq 2 -and
           [AumQaOsInput]::QueryCta($main,5) -eq 1 -and
           [AumQaOsInput]::QueryCta($main,21) -eq 3){
            $mixedReady=$true;break
        }
        Start-Sleep -Milliseconds 70
    }
    if(-not $mixedReady){throw 'Mixed-state weekly/session CTA order not observed'}
    if([AumQaWin32]::Find($proc.Id,'#32770') -ne [IntPtr]::Zero){
        throw 'Mixed-state fixture opened dialog before cell click'
    }
    $mixedCases=@(
        @{ name='session'; index=1; action=3; title='AI provider connection diagnosis'; text='Retrieval failed. Check authentication' },
        @{ name='weekly'; index=0; action=1; title='QA auth dispatch intercepted'; text='qa_claude_login_dispatch_blocked' }
    )
    foreach($case in $mixedCases){
        $idx=[int]$case.index
        $action=[int]$case.action
        if([AumQaOsInput]::QueryCta($main,1) -ne 2 -or
           [AumQaOsInput]::QueryCta($main,5+16*$idx) -ne $action){
            throw ('Cell action or count changed before click: '+$case.name)
        }
        $l=[AumQaOsInput]::QueryCta($main,6+16*$idx)
        $t=[AumQaOsInput]::QueryCta($main,7+16*$idx)
        $r=[AumQaOsInput]::QueryCta($main,8+16*$idx)
        $b=[AumQaOsInput]::QueryCta($main,9+16*$idx)
        $visibleClient=New-Object AumQaWin32+RECT
        $visibleWindow=New-Object AumQaWin32+RECT
        if(-not [AumQaWin32]::GetClientRect($main,[ref]$visibleClient) -or
           -not [AumQaWin32]::GetWindowRect($main,[ref]$visibleWindow)){
            throw 'Mixed-state window geometry unavailable'
        }
        $evidence.mixed_geometry+=@{
            name=$case.name
            target=@{left=$l;top=$t;right=$r;bottom=$b}
            client=@{width=$visibleClient.Right;height=$visibleClient.Bottom}
            window=@{left=$visibleWindow.Left;top=$visibleWindow.Top;right=$visibleWindow.Right;bottom=$visibleWindow.Bottom}
            index=$idx
            target_inside_client=(($l -ge 0) -and ($t -ge 0) -and ($r -le $visibleClient.Right) -and ($b -le $visibleClient.Bottom))
        }
        $mode=[AumQaOsInput]::HoverClickOwnedCta(
            $main,$l,$t,$r,$b,$proc.Id,$action,$idx,2)
        $clickedDialog=[IntPtr]::Zero
        for($i=0;$i -lt 100;$i++){
            if($proc.HasExited){throw ('App exited after mixed cell click '+$case.name)}
            $clickedDialog=[AumQaWin32]::Find($proc.Id,'#32770')
            if($clickedDialog -ne [IntPtr]::Zero){break}
            Start-Sleep -Milliseconds 100
        }
        if($clickedDialog -eq [IntPtr]::Zero){
            throw ('No dialog after clicking '+$case.name)
        }
        $title=[AumQaWin32]::Title($clickedDialog)
        $body=[AumQaWin32]::DialogText($clickedDialog)
        if($title -ne [string]$case.title -or
           [AumQaWin32]::GetWindow($clickedDialog,4) -ne $main -or
           -not $body.Contains([string]$case.text)){
            throw ('Wrong state dispatched for '+$case.name)
        }
        if($case.name -eq 'session' -and $body.Contains('Check authentication. Click')){
            throw 'Session click incorrectly selected weekly auth-expired state'
        }
        $evidence.cell_target_cases+=@{
            name=$case.name
            status='pass'
            simultaneous_window_states=$true
            selected_target_index=$idx
            click_mode=$mode
            product_hover_verified=$true
            selected_action_code=$action
            correct_dialog_content_verified=$true
            external_auth_launch_intercepted=$true
            no_real_user_auth_tested=$true
        }
        if(-not [AumQaWin32]::PostMessageW($clickedDialog,0x0010,[IntPtr]::Zero,[IntPtr]::Zero)){
            throw ('Could not close mixed-state '+$case.name+' dialog')
        }
        for($i=0;$i -lt 50;$i++){
            if([AumQaWin32]::Find($proc.Id,'#32770') -eq [IntPtr]::Zero){break}
            Start-Sleep -Milliseconds 100
        }
        if([AumQaWin32]::Find($proc.Id,'#32770') -ne [IntPtr]::Zero){
            throw ('Mixed-state '+$case.name+' dialog did not close')
        }
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
    # The only moved window is this QA process's own popup. No taskbar,
    # global display setting, other app, or actual user pointer is modified.
    $baseRect=New-Object AumQaWin32+RECT
    if(-not [AumQaWin32]::GetWindowRect($main,[ref]$baseRect)){
        throw 'Unable to measure QA popup baseline'
    }
    $area=[AumQaWin32]::WorkArea($main)
    $gapRight=[Math]::Abs($area.Right-$baseRect.Right)
    $gapBottom=[Math]::Abs($area.Bottom-$baseRect.Bottom)
    if($gapRight -gt 12 -or $gapBottom -gt 12){
        throw ('Initial popup not anchored at work-area lower-right. gaps='+$gapRight+','+$gapBottom)
    }
    if(($baseRect.Left-$area.Left) -lt 112 -or ($baseRect.Top-$area.Top) -lt 95){
        throw 'VM work area too small for safe own-popup displacement'
    }
    $swpFlags=[uint32]0x0015 # SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE
    $shiftX=73
    $shiftY=47
    function GetQaRect {
        $r=New-Object AumQaWin32+RECT
        if(-not [AumQaWin32]::GetWindowRect($main,[ref]$r)){
            throw 'GetWindowRect of own QA popup failed'
        }
        return $r
    }
    function WaitQaPosition([int]$x,[int]$y,[string]$label) {
        for($i=0;$i -lt 80;$i++){
            if($proc.HasExited){throw ('QA process exited: '+$label)}
            $r=GetQaRect
            if([Math]::Abs($r.Left-$x) -le 3 -and [Math]::Abs($r.Top-$y) -le 3){
                return $r
            }
            Start-Sleep -Milliseconds 100
        }
        $r=GetQaRect
        throw ('Popup position mismatch: '+$label+' expected='+$x+','+$y+' actual='+$r.Left+','+$r.Top)
    }
    if(-not [AumQaWin32]::SetWindowPos($main,[IntPtr]::Zero,$baseRect.Left-$shiftX,$baseRect.Top-$shiftY,0,0,$swpFlags)){
        throw 'Failed to move QA popup within its own work area'
    }
    $null=WaitQaPosition ($baseRect.Left-$shiftX) ($baseRect.Top-$shiftY) 'displaced for tray event'
    # 0x8005 is native_interop::WM_APP_TRAY_REPOSITION. Verify the actual
    # app message handler returns popup to the lower-right work-area anchor.
    if(-not [AumQaWin32]::PostMessageW($main,0x8005,[IntPtr]::Zero,[IntPtr]::Zero)){
        throw 'App tray-reposition message rejected'
    }
    # The QA fixtures may have changed the provider count and row height.
    # position_at_taskbar() intentionally resolves the CURRENT widget size.
    # Reusing the initial top-left coordinate is invalid whenever width or
    # height changes; validate the work-area anchor instead.
    function WaitQaAnchor([string]$label) {
        for($i=0;$i -lt 80;$i++) {
            if($proc.HasExited){throw ('QA process exited: '+$label)}
            $r=GetQaRect
            if([Math]::Abs($r.Right-$area.Right) -le 12 -and
               [Math]::Abs($r.Bottom-$area.Bottom) -le 12) {
                return $r
            }
            Start-Sleep -Milliseconds 100
        }
        $r=GetQaRect
        throw ('Popup not anchored: '+$label+' expectedRightBottom='+$area.Right+','+$area.Bottom+
            ' observedRect='+$r.Left+','+$r.Top+','+$r.Right+','+$r.Bottom)
    }
    $restored=WaitQaAnchor 'tray-reposition handler'
    if(($restored.Left-$area.Left) -lt ($shiftX+5) -or
       ($restored.Top-$area.Top) -lt ($shiftY+5)){
        throw 'QA popup too close to upper-left for second displacement'
    }
    if(-not [AumQaWin32]::SetWindowPos($main,[IntPtr]::Zero,$restored.Left-$shiftX,$restored.Top-$shiftY,0,0,$swpFlags)){
        throw 'Second QA popup displacement failed'
    }
    $null=WaitQaPosition ($restored.Left-$shiftX) ($restored.Top-$shiftY) 'displaced for display notification'
    # WM_DISPLAYCHANGE=0x007E, delivered ONLY to this isolated popup. Does
    # not change resolution or broadcast anything to the runner desktop.
    if(-not [AumQaWin32]::PostMessageW($main,0x007E,[IntPtr]::Zero,[IntPtr]::Zero)){
        throw 'Popup-specific display-change message rejected'
    }
    $displayRestored=WaitQaAnchor 'display-change handler'
    $evidence.popup_position=[ordered]@{
        status='pass'
        initial_anchor_work_area='lower_right'
        tray_reposition_handler='pass'
        displaychange_handler='pass'
        work_area_right=$area.Right
        work_area_bottom=$area.Bottom
        initial_left=$baseRect.Left
        initial_top=$baseRect.Top
        displaced_pixels_x=$shiftX
        displaced_pixels_y=$shiftY
        restored_left=$restored.Left
        restored_top=$restored.Top
        initial_width=$baseRect.Right-$baseRect.Left
        initial_height=$baseRect.Bottom-$baseRect.Top
        restored_width=$restored.Right-$restored.Left
        restored_height=$restored.Bottom-$restored.Top
        initial_position_changed_with_geometry=(($restored.Left -ne $baseRect.Left) -or ($restored.Top -ne $baseRect.Top))
        display_restored_left=$displayRestored.Left
        display_restored_top=$displayRestored.Top
        real_taskbar_moved=$false
        natural_winevent_tested=$false
    }
    # QA-only OS WinEvent dispatch probe: move ONLY a window owned by this
    # isolated test process. The probe callback posts WM_APP_TRAY_REPOSITION
    # to the actual product popup, without touching Explorer's tray geometry.
    # This verifies the OS delivery + message-handler bridge, NOT the
    # production hook's Explorer-thread filter or natural taskbar movement.
    if(-not [AumQaWin32]::SetWindowPos(
        $main,[IntPtr]::Zero,
        $displayRestored.Left-$shiftX,$displayRestored.Top-$shiftY,
        0,0,$swpFlags)){
        throw 'Unable to displace QA popup before own-window WinEvent'
    }
    $null=WaitQaPosition ($displayRestored.Left-$shiftX) ($displayRestored.Top-$shiftY) 'before own-window event'
    Start-Sleep -Milliseconds 950
    $control=GetQaRect
    if([Math]::Abs($control.Left-($displayRestored.Left-$shiftX)) -gt 3 -or
       [Math]::Abs($control.Top-($displayRestored.Top-$shiftY)) -gt 3){
        throw 'Unexpected popup movement before own-window event'
    }
    if([AumOwnWindowWinEventProbe]::Run($main) -ne 'pass'){
        throw 'Owned-window WinEvent probe unsuccessful'
    }
    $eventRestored=WaitQaAnchor 'OS WinEvent from owned helper window to product message handler'
    $evidence.popup_position.natural_winevent_tested=$false
    $evidence.popup_position.own_window_winevent_tested=$true
    $evidence.popup_position.own_window_dispatch_to_product_handler='pass'
    $evidence.popup_position.explorer_tray_touched=$false
    $evidence.popup_position.event_restored_left=$eventRestored.Left
    $evidence.popup_position.event_restored_top=$eventRestored.Top
    $evidence.status='pass_help_diagnostics_antigravity_reposition_own_window_probe'
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
    if($evidence.status -eq 'pass_help_diagnostics_antigravity_reposition_own_window_probe' -and
       (-not $evidence.process_exited -or -not $evidence.env_scrubbed -or -not $evidence.dialog_owner_verified)) {
        $evidence.status='fail'
        $evidence.note='Cleanup or isolation not verified.'
    }
    $evidence|ConvertTo-Json -Depth 5|Set-Content -LiteralPath $EvidencePath -Encoding UTF8
    if($evidence.process_exited -and (Test-Path $sandbox)) {Remove-Item $sandbox -Recurse -Force}
    Write-Output ("GUI_EVIDENCE_STATUS="+$evidence.status)
    Write-Output ("GUI_EVIDENCE_PATH="+$EvidencePath)
}
