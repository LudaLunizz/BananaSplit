# Launches the real bananasplit.exe the way a user does and checks that it
# works as a standalone Windows app:
# - the exe: a Windows (GUI) program with the app icon and version info,
#   whose command-line options still print;
# - a double-click start opens the mixer, its window and its tray icon; a
#   second start brings that window up instead of starting a second mixer;
# - the close button hides the window in the tray (BananaSplit keeps
#   running); the tray icon brings it back, and records;
# - --quit, the tray's Quit, `taskkill` (without /f) and the end of the
#   Windows session all save and close everything, the window too;
# - the autostart start (--no-open) shows only the tray icon;
# - with "Closing the window keeps it running" off, closing the window quits.
# Run on a Windows desktop session (CI: windows-latest).
#
#   pwsh windows/tests/launch/smoke.ps1 -Exe path\to\bananasplit.exe
#
# Uses a throw-away profile folder, so the real settings are not touched.
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [int]$Port = 8790
)
$ErrorActionPreference = 'Continue'
$Exe = (Resolve-Path $Exe).Path
$base = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { $env:TEMP }
$work = Join-Path $base 'bs-smoke'
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$work\Roaming", "$work\Local" | Out-Null
$env:APPDATA = "$work\Roaming"
$env:LOCALAPPDATA = "$work\Local"
$url = "http://127.0.0.1:$Port"

$script:checks = 0
$script:failures = 0
# On GitHub Actions, failures and the app's own messages also become
# annotations, shown on the run's summary page (no need to open the log).
$script:onGitHub = $env:GITHUB_ACTIONS -eq 'true'
function Annotate([string]$level, [string]$title, [string]$text) {
    if (-not $script:onGitHub -or -not $text) { return }
    $t = $text.Trim()
    if ($t.Length -gt 4000) { $t = $t.Substring(0, 4000) + ' ...' }
    $t = $t.Replace('%', '%25').Replace("`r", '%0D').Replace("`n", '%0A')
    Write-Output "::${level} title=${title}::${t}"
}
function Check([bool]$ok, [string]$what) {
    $script:checks++
    if ($ok) { Write-Output "ok   $what" } else {
        $script:failures++
        Write-Output "FAIL $what"
        Annotate 'error' 'Windows launch' $what
    }
}
function Until([scriptblock]$cond, [int]$seconds = 20) {
    $t = [Diagnostics.Stopwatch]::StartNew()
    while ($t.Elapsed.TotalSeconds -lt $seconds) {
        try { if (& $cond) { return $true } } catch { }
        Start-Sleep -Milliseconds 250
    }
    return $false
}
function Get-Http([string]$path) {
    (Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 -ErrorAction Stop "$url$path").Content
}
function Mixers { @(Get-Process bananasplit -ErrorAction SilentlyContinue) }
function Screenshot([string]$name) {
    try {
        Add-Type -AssemblyName System.Windows.Forms, System.Drawing
        $b = [Windows.Forms.SystemInformation]::VirtualScreen
        $bmp = New-Object Drawing.Bitmap $b.Width, $b.Height
        $g = [Drawing.Graphics]::FromImage($bmp)
        $g.CopyFromScreen($b.Left, $b.Top, 0, 0, $bmp.Size)
        $bmp.Save("$work\$name.png", [Drawing.Imaging.ImageFormat]::Png)
        $g.Dispose(); $bmp.Dispose()
        Write-Output "screenshot: $work\$name.png ($($b.Width)x$($b.Height))"
    } catch { Write-Output "screenshot failed: $_" }
}
function Windows-Titled([string]$title) {
    @(Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowTitle -eq $title })
}
# The app's windows, found by title. Not Process.MainWindowTitle: that is
# the first visible window of a process in z-order, and once the app window
# is minimized it can be tao's (invisible, untitled) event window instead.
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
namespace BsSmoke {
    public static class Win {
        [DllImport("user32.dll", SetLastError = true)] public static extern bool PostMessage(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
        [DllImport("user32.dll", SetLastError = true)] public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam, uint flags, uint timeout, out IntPtr result);
        [DllImport("shell32.dll", CharSet = CharSet.Unicode)] public static extern uint ExtractIconEx(string file, int index, IntPtr[] large, IntPtr[] small, uint count);
        [StructLayout(LayoutKind.Sequential)] public struct NotifyIconIdentifier { public uint cbSize; public IntPtr hWnd; public uint uID; public Guid guidItem; }
        [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
        [DllImport("shell32.dll")] static extern int Shell_NotifyIconGetRect(ref NotifyIconIdentifier id, out Rect rect);
        // Is BananaSplit's icon (id 1 of this window) in the notification area?
        public static bool TrayIconShown(IntPtr hWnd) {
            var id = new NotifyIconIdentifier { cbSize = (uint)Marshal.SizeOf(typeof(NotifyIconIdentifier)), hWnd = hWnd, uID = 1, guidItem = Guid.Empty };
            Rect r;
            return Shell_NotifyIconGetRect(ref id, out r) == 0;
        }
        // Top-level windows of this class (visible or not).
        public static IntPtr[] OfClass(string name) {
            var found = new List<IntPtr>();
            EnumWindows((h, l) => {
                var cls = new StringBuilder(256);
                GetClassName(h, cls, cls.Capacity);
                if (cls.ToString() == name) found.Add(h);
                return true;
            }, IntPtr.Zero);
            return found.ToArray();
        }
        [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);
        [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
        [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
        [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
        delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
        [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc proc, IntPtr lParam);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr hWnd, StringBuilder text, int max);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hWnd, StringBuilder name, int max);
        // Visible top-level windows with this title (minimized ones too), not
        // dialog boxes (an error message is titled BananaSplit too).
        public static IntPtr[] Titled(string title) {
            var found = new List<IntPtr>();
            EnumWindows((h, l) => {
                if (IsWindowVisible(h)) {
                    var sb = new StringBuilder(256);
                    GetWindowText(h, sb, sb.Capacity);
                    var cls = new StringBuilder(256);
                    GetClassName(h, cls, cls.Capacity);
                    if (sb.ToString() == title && cls.ToString() != "#32770") found.Add(h);
                }
                return true;
            }, IntPtr.Zero);
            return found.ToArray();
        }
        // Message boxes (#32770) of this process.
        public static IntPtr[] Dialogs(uint pid) {
            var found = new List<IntPtr>();
            EnumWindows((h, l) => {
                var cls = new StringBuilder(256);
                GetClassName(h, cls, cls.Capacity);
                uint p;
                GetWindowThreadProcessId(h, out p);
                if (cls.ToString() == "#32770" && p == pid && IsWindowVisible(h)) found.Add(h);
                return true;
            }, IntPtr.Zero);
            return found.ToArray();
        }
        public static uint ProcessOf(IntPtr hWnd) {
            uint pid;
            GetWindowThreadProcessId(hWnd, out pid);
            return pid;
        }
    }
}
'@
# BananaSplit's windows (of bananasplit processes, titled BananaSplit).
function App-Windows {
    @([BsSmoke.Win]::Titled('BananaSplit') | Where-Object {
        (Get-Process -Id ([BsSmoke.Win]::ProcessOf($_)) -ErrorAction SilentlyContinue).ProcessName -eq 'bananasplit'
    })
}
function App-Window { App-Windows | Select-Object -First 1 }
# What the window's close button (or Alt+F4) sends.
function Close-Window {
    $h = App-Window
    if (-not $h) { return 'no window to close' }
    $ok = [BsSmoke.Win]::PostMessage($h, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
    if ($ok) { 'WM_CLOSE posted' } else { "PostMessage failed (error $([Runtime.InteropServices.Marshal]::GetLastWin32Error()))" }
}
function Window-Minimized {
    $h = App-Window
    [bool]($h -and [BsSmoke.Win]::IsIconic($h))
}
# A window of a bananasplit process, by class: the tray icon's window (in
# the window process) and the mixer's hidden one (session end, taskkill).
function Class-Window([string]$cls) {
    @([BsSmoke.Win]::OfClass($cls) | Where-Object {
        (Get-Process -Id ([BsSmoke.Win]::ProcessOf($_)) -ErrorAction SilentlyContinue).ProcessName -eq 'bananasplit'
    }) | Select-Object -First 1
}
function Tray-Window { Class-Window 'BananaSplitTray' }
function Mixer-Window { Class-Window 'BananaSplitMixer' }
function Send([IntPtr]$h, [uint32]$msg, [long]$w, [long]$l, [int]$ms = 10000) {
    $r = [IntPtr]::Zero
    $ok = [BsSmoke.Win]::SendMessageTimeout($h, $msg, [IntPtr]$w, [IntPtr]$l, 2, $ms, [ref]$r)
    if ($ok -eq [IntPtr]::Zero) { return $null }
    return $r.ToInt64()
}
# A left click on the tray icon (what Explorer sends the icon's window).
function Tray-Click { $t = Tray-Window; if ($t) { [void](Send $t 0x8001 1 0x0202) } }
# A tray menu item: 1 Show, 2 Start/Stop recording, 3 Quit.
function Tray-Item([int]$id) { $t = Tray-Window; if ($t) { [void][BsSmoke.Win]::PostMessage($t, 0x0111, [IntPtr]$id, [IntPtr]::Zero) } }
function Start-App([string[]]$argv, [string]$log) {
    $a = @{ FilePath = $Exe; PassThru = $true; RedirectStandardError = "$work\$log.err"; RedirectStandardOutput = "$work\$log.out" }
    if ($argv) { $a.ArgumentList = $argv }
    Start-Process @a
}
function Saved-Ini { Get-Content -Raw "$work\Roaming\bananasplit\bananasplit.ini" -ErrorAction SilentlyContinue }
function Wait-Gone($p, [string]$what) {
    $ended = $p.WaitForExit(15000)
    Check $ended "$what"
    Check (Until { (Mixers).Count -eq 0 } 15) "no BananaSplit process is left (window closed too)"
    if ($ended) { Check ($p.ExitCode -eq 0) "the mixer exit code is 0 (was $($p.ExitCode))" }
    Get-Process bananasplit -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

# For the annotations when a window check fails.
function Window-State {
    $fg = [BsSmoke.Win]::GetForegroundWindow()
    $fgPid = [BsSmoke.Win]::ProcessOf($fg)
    $fgName = (Get-Process -Id $fgPid -ErrorAction SilentlyContinue).ProcessName
    $all = App-Windows
    $mine = if ($all.Count) {
        ($all | ForEach-Object { "window pid $([BsSmoke.Win]::ProcessOf($_)) hwnd $_ minimized $([BsSmoke.Win]::IsIconic($_))" }) -join ', '
    } else { 'no window titled BananaSplit' }
    "$mine; foreground: $fgName [$fgPid] hwnd $fg"
}
function Post-Cmd([string]$body) {
    (Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 -ErrorAction Stop -Method Post -Uri "$url/api/cmd" `
        -Headers @{ 'X-BananaSplit' = '1' } -ContentType 'application/x-www-form-urlencoded' -Body $body).Content
}
# A mixer started when the default port was taken uses another one.
function Use-NotedPort {
    $f = Get-ChildItem "$work\Local\bananasplit\*.port" -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($f) {
        $n = (Get-Content -Raw $f.FullName).Trim()
        if ($n -match '^\d+$') { $script:url = "http://127.0.0.1:$n" }
    }
}

# ---- the exe: a Windows program, with its icon and version ----
$bytes = [IO.File]::ReadAllBytes($Exe)
$pe = [BitConverter]::ToInt32($bytes, 0x3C)
$subsystem = [BitConverter]::ToUInt16($bytes, $pe + 0x5C)
Check ($subsystem -eq 2) "bananasplit.exe is a Windows (GUI) program, so no console window opens (subsystem $subsystem)"
$icons = [BsSmoke.Win]::ExtractIconEx($Exe, -1, $null, $null, 0)
Check ($icons -ge 1) "it has the app icon ($icons icon group(s))"
$vi = (Get-Item $Exe).VersionInfo
Check ($vi.ProductName -eq 'BananaSplit' -and $vi.ProductVersion -match '^\d+\.\d+\.\d+' -and $vi.FileVersion -match '^\d+\.\d+\.\d+\.\d+$') "it has version information ($($vi.ProductName) $($vi.ProductVersion), file $($vi.FileVersion))"

# ---- the command line still works (output to a pipe, as from a script) ----
$v = (& $Exe --version 2>&1 | Out-String)
Check ($LASTEXITCODE -eq 0 -and $v -match 'BananaSplit \d+\.\d+\.\d+') "--version prints the version ($($v.Trim()))"
Check ($v -match [regex]::Escape($vi.ProductVersion)) "the same version as the file's version information"
$d = (& $Exe --print-default-config 2>&1 | Out-String)
Check ($LASTEXITCODE -eq 0 -and $d -match '\[strip\.1\]|\[strip1\]|\[global\]') "--print-default-config prints a config"
$l = (& $Exe --list-devices 2>&1 | Out-String)
Write-Output "--list-devices (exit $LASTEXITCODE):"; Write-Output $l
Annotate 'notice' "--list-devices (exit $LASTEXITCODE)" $l

# ---- "Start with Windows" left by an older copy elsewhere ----
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
# (A fresh profile may not have the Run key yet.)
if (-not (Test-Path $run)) { New-Item -Path $run | Out-Null }
$savedRun = (Get-ItemProperty -Path $run -Name BananaSplit -ErrorAction SilentlyContinue).BananaSplit
New-ItemProperty -Path $run -Name BananaSplit -Value '"C:\Old\bananasplit-2.7.0\bananasplit.exe" --gui --no-open' -PropertyType String -Force | Out-Null

# ---- a double-click start: no arguments (the first start on this PC) ----
$p = Start-App @() 'mixer'
Check (Until { (Get-Http '/api/ping') -eq 'bananasplit-gui' }) "the mixer answers on $url"
$state = $null
Check (Until { $script:state = Get-Http '/api/state' | ConvertFrom-Json; $null -ne $script:state.ch }) "it publishes its state (valid JSON, with the channels)"
if ($state) {
    $line = "state: rate={0} block={1} cables={2} status='{3}' channels={4}" -f $state.sr, $state.bs, $state.cables, $state.status, @($state.ch).Count
    Write-Output $line
    Annotate 'notice' 'Mixer state' $line
}
Check (Until { (App-Windows).Count -ge 1 } 30) "a window titled BananaSplit opens"
# The first start, without VB-Audio cables (the runner has none): the
# welcome dialog offers them, and what happened at start is on the page.
$welcome = $null
Check (Until { $script:welcome = [BsSmoke.Win]::Dialogs([uint32]$p.Id) | Select-Object -First 1; $null -ne $script:welcome } 15) "the first start offers the VB-Audio cables in a dialog"
if ($welcome) { [void][BsSmoke.Win]::PostMessage($welcome, 0x0111, [IntPtr]7, [IntPtr]::Zero) } # No
Check (Until { [BsSmoke.Win]::Dialogs([uint32]$p.Id).Count -eq 0 } 10) "the dialog closes"
$st0 = Get-Http '/api/state' | ConvertFrom-Json
Check ($st0.status -match 'VB-Audio') "the start-up notes are in the page's status line ('$($st0.status)')"
Check (Until { @(Get-NetTCPConnection -LocalPort $Port -State Established -ErrorAction SilentlyContinue).Count -ge 1 } 30) "the window's page connects to the mixer"
Check (Until { $t = Tray-Window; $t -and [BsSmoke.Win]::TrayIconShown($t) } 15) "the tray icon is in the notification area"
Check (Until { (Get-Http '/api/state' | ConvertFrom-Json).app.tray -eq $true } 10) "the page knows there is a tray icon"
Check ($null -ne (Mixer-Window)) "the mixer listens for the end of the Windows session"
$auto0 = (Get-Http '/api/state' | ConvertFrom-Json).app.autostart
$regq = (& reg.exe query 'HKCU\Software\Microsoft\Windows\CurrentVersion\Run' /v BananaSplit 2>&1 | Out-String).Trim()
Annotate 'notice' 'Run entry at start' "app.autostart=$auto0; reg: $regq"
Check ($auto0 -eq $false) "a Run entry of an older copy does not count as 'Start with Windows'"
try { [void](Post-Cmd 'op=autostart&v=1') } catch { }
$entry = (Get-ItemProperty -Path $run -Name BananaSplit -ErrorAction SilentlyContinue).BananaSplit
Check ($entry -like "*$Exe*" -and (Until { (Get-Http '/api/state' | ConvertFrom-Json).app.autostart -eq $true } 10)) "ticking it points the entry at this program ($entry)"
try { [void](Post-Cmd 'op=autostart&v=0') } catch { }
Check ($null -eq (Get-ItemProperty -Path $run -Name BananaSplit -ErrorAction SilentlyContinue)) "unticking it removes the entry"

# ---- other users of this PC can't drive the mixer ----
$other = 'bs-other'
# 14 characters at most: net user asks before accepting a longer one.
$pw = 'Bs!' + [guid]::NewGuid().ToString('N').Substring(0, 9) + 'a1'
$made = (& net.exe user $other $pw /add /y 2>&1 | Out-String)
if ($LASTEXITCODE -ne 0) { Annotate 'warning' 'net user' $made }
$out = 'C:\Users\Public\bs-other.txt'
Remove-Item $out -ErrorAction SilentlyContinue
$cmd = "& { try { (Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 '$url/api/state').StatusCode } catch { if (`$_.Exception.Response) { [int]`$_.Exception.Response.StatusCode } else { 'error: ' + `$_.Exception.Message } } } | Out-File -Encoding ascii '$out'"
try {
    $cred = New-Object Management.Automation.PSCredential ($other, (ConvertTo-SecureString $pw -AsPlainText -Force))
    $o = Start-Process powershell.exe -Credential $cred -LoadUserProfile -WindowStyle Hidden -PassThru -Wait -ArgumentList '-NoProfile', '-Command', $cmd
    $said = (Get-Content -Raw $out -ErrorAction SilentlyContinue)
    $said = if ($said) { $said.Trim() } else { "nothing (exit $($o.ExitCode))" }
    Check ($said -eq '403') "another Windows user is refused ($said)"
} catch { Check $false "another Windows user is refused (could not run as them: $_)" }
& net.exe user $other /delete 2>&1 | Out-Null
Check ((Get-Http '/api/ping') -eq 'bananasplit-gui') "and this user still gets in"
Start-Sleep -Seconds 3
Screenshot 'launch'
Check (-not $p.HasExited) "the mixer keeps running"

# ---- starting it again brings up the running one ----
$p2 = Start-App @() 'second'
Check ($p2.WaitForExit(15000) -and $p2.ExitCode -eq 0) "a second start hands over to the running mixer and exits"
Check ((Get-Http '/api/ping') -eq 'bananasplit-gui' -and -not $p.HasExited) "the first mixer is still the one running"
Start-Sleep -Seconds 2
Check ((App-Windows).Count -eq 1) "it brought up the open window instead of opening another"

# ---- the close button hides the window in the tray ----
$before = Window-State
$posted = Close-Window
$hidden = Until { (App-Windows).Count -eq 0 } 10
Check $hidden "closing the window hides it (in the tray)"
if (-not $hidden) { Annotate 'warning' 'Window state' "before: $before`n$posted`nafter: $(Window-State)" }
Start-Sleep -Seconds 1
Check (-not $p.HasExited -and (Mixers).Count -ge 2) "and the mixer and its window keep running"
Check ((Tray-Window) -and [BsSmoke.Win]::TrayIconShown((Tray-Window))) "the tray icon stays"
Tray-Click
$back = Until { (App-Windows).Count -eq 1 -and -not (Window-Minimized) } 10
Check $back "a click on the tray icon brings the window back"
if (-not $back) { Annotate 'warning' 'Window state' (Window-State) }
[void](Close-Window)
Check (Until { (App-Windows).Count -eq 0 } 10) "closed again, it is back in the tray"
$p3 = Start-App @() 'third'
Check ($p3.WaitForExit(15000) -and $p3.ExitCode -eq 0) "starting BananaSplit again"
$back = Until { (App-Windows).Count -eq 1 -and -not (Window-Minimized) } 10
Check $back "brings the window back"
if (-not $back) { Annotate 'warning' 'Window state' (Window-State) }
Screenshot 'restored'

# ---- the tray menu records ----
# Takes go to the test folder; A1 is armed.
try { [void](Post-Cmd ('op=recset&folder=' + [uri]::EscapeDataString("$work\takes"))) } catch { }
try { [void](Post-Cmd 'op=link&ch=6&v=1') } catch { }
Tray-Item 2
Check (Until { (Get-Http '/api/state' | ConvertFrom-Json).rec.on -eq $true } 10) "the tray's Start recording starts a take"
Tray-Item 2
Check (Until { (Get-Http '/api/state' | ConvertFrom-Json).rec.on -eq $false } 10) "and its Stop recording stops it"
$takes = @(Get-ChildItem -Recurse -Filter *.wav "$work\takes" -ErrorAction SilentlyContinue)
Check ($takes.Count -ge 1) "the take was written ($(($takes | Select-Object -First 1).Name))"

# ---- quitting saves and closes everything ----
$q = Start-Process -FilePath $Exe -ArgumentList '--quit' -PassThru -Wait -RedirectStandardError "$work\quit.err" -RedirectStandardOutput "$work\quit.out"
Check ($q.ExitCode -eq 0) "--quit is accepted"
Wait-Gone $p "the mixer exits after --quit"
Check (Test-Path "$work\Roaming\bananasplit\bananasplit.ini") "the settings were saved"

# ---- the autostart start: only the tray icon ----
$p5 = Start-App @('--gui', '--no-open') 'hidden'
Check (Until { Use-NotedPort; (Get-Http '/api/ping') -eq 'bananasplit-gui' }) "started in the background (--no-open), the mixer answers"
Check (Until { $t = Tray-Window; $t -and [BsSmoke.Win]::TrayIconShown($t) } 30) "and its tray icon shows"
Start-Sleep -Seconds 2
Check ((App-Windows).Count -eq 0) "but no window"
$p6 = Start-App @() 'hidden2'
Check ($p6.WaitForExit(15000) -and $p6.ExitCode -eq 0) "starting BananaSplit then"
Check (Until { (App-Windows).Count -eq 1 -and -not (Window-Minimized) } 15) "shows its window"

# ---- the end of the Windows session saves the settings ----
try { [void](Post-Cmd 'op=gain&ch=0&v=-7.5') } catch { }
$m = Mixer-Window
Check ($null -ne $m) "the mixer's session window is there"
if ($m) {
    $query = Send $m 0x0011 0 0x80000000
    Check ($query -eq 1) "logging off: the mixer does not hold it up (WM_QUERYENDSESSION answered $query)"
    $t0 = [Diagnostics.Stopwatch]::StartNew()
    $end = Send $m 0x0016 1 0x80000000 15000
    Check ($null -ne $end -and $t0.Elapsed.TotalSeconds -lt 6) ("WM_ENDSESSION returns within the time Windows allows ({0:N1} s)" -f $t0.Elapsed.TotalSeconds)
    Check ((Saved-Ini) -match 'gain_db = -7\.5') "the change made just before was saved"
}
Wait-Gone $p5 "the mixer ends with the session"

# ---- taskkill (without /f) quits cleanly ----
$p7 = Start-App @() 'taskkill'
Check (Until { Use-NotedPort; (Get-Http '/api/ping') -eq 'bananasplit-gui' }) "started again, the mixer answers"
Check (Until { (App-Windows).Count -ge 1 } 30) "and opens its window"
try { [void](Post-Cmd 'op=gain&ch=0&v=-3') } catch { }
& taskkill.exe /pid $p7.Id | Out-Null
Wait-Gone $p7 "taskkill /pid (without /f) quits it"
Check ((Saved-Ini) -match 'gain_db = -3\r?\n') "and it saved first"

# ---- the tray's Quit ----
$p8 = Start-App @() 'trayquit'
Check (Until { Use-NotedPort; (Get-Http '/api/ping') -eq 'bananasplit-gui' }) "started again, the mixer answers"
Check (Until { $t = Tray-Window; $t -and [BsSmoke.Win]::TrayIconShown($t) } 30) "with its tray icon"
$tw = Tray-Window
Tray-Item 3
Wait-Gone $p8 "the tray's Quit quits BananaSplit"
Check (-not [BsSmoke.Win]::TrayIconShown($tw)) "and its icon left the notification area"

# ---- with "Closing the window keeps it running" off, closing it quits ----
$p4 = Start-App @() 'mixer2'
Check (Until { Use-NotedPort; (Get-Http '/api/ping') -eq 'bananasplit-gui' }) "started again, the mixer answers on $url"
Check (Until { (App-Windows).Count -ge 1 } 30) "and opens its window"
$said = $null
try { $said = Post-Cmd 'op=closetray&v=0' } catch { }
# The mixer passes it on to the window in the frame that publishes it (once
# the window has started).
Check ($said -eq 'ok' -and (Until { (Get-Http '/api/state' | ConvertFrom-Json).app.closeToTray -eq $false } 20)) "the menu's 'Closing the window keeps it running' is switched off"
Start-Sleep -Seconds 1
$before = Window-State
$posted = Close-Window
$quit = $p4.WaitForExit(15000)
Check $quit "closing the window quits BananaSplit"
if (-not $quit) { Annotate 'warning' 'Window state' "before: $before`n$posted`nafter: $(Window-State)" }
Check (Until { (Mixers).Count -eq 0 } 15) "no BananaSplit process is left"
Check ((Saved-Ini) -match 'close_to_tray = false') "the setting was saved"
if ($p4.HasExited) { Check ($p4.ExitCode -eq 0) "the mixer exit code is 0 (was $($p4.ExitCode))" }

# ---- at login, the taskbar may come after BananaSplit ----
# Windows starts the autostart entry while Explorer is still starting. Here:
# no Explorer at all (Windows must not restart it by itself meanwhile).
$winlogon = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon'
$ars = (Get-ItemProperty -Path $winlogon -Name AutoRestartShell -ErrorAction SilentlyContinue).AutoRestartShell
Set-ItemProperty -Path $winlogon -Name AutoRestartShell -Value 0 -Type DWord -ErrorAction SilentlyContinue
Stop-Process -Name explorer -Force -ErrorAction SilentlyContinue
Check (Until { @(Get-Process explorer -ErrorAction SilentlyContinue).Count -eq 0 } 15) "Explorer is stopped (no taskbar, as early at login)"
$p9 = Start-App @('--gui', '--no-open') 'login'
Check (Until { Use-NotedPort; (Get-Http '/api/ping') -eq 'bananasplit-gui' }) "started in the background before the taskbar, the mixer answers"
Start-Sleep -Seconds 5
Check ((App-Windows).Count -eq 0) "it shows no window while it waits for the taskbar"
Check ($null -ne (Tray-Window)) "its tray icon's window waits too"
Start-Process explorer.exe
if ($null -ne $ars) { Set-ItemProperty -Path $winlogon -Name AutoRestartShell -Value $ars -Type DWord -ErrorAction SilentlyContinue }
else { Remove-ItemProperty -Path $winlogon -Name AutoRestartShell -ErrorAction SilentlyContinue }
$came = Until { $t = Tray-Window; $t -and [BsSmoke.Win]::TrayIconShown($t) } 45
Check $came "once the taskbar is there, the tray icon appears"
Check ((App-Windows).Count -eq 0) "and the window stays in the tray"
try { [void](Post-Cmd 'op=quit') } catch { }
Wait-Gone $p9 "it quits"
Screenshot 'after-explorer'

$logs = [ordered]@{ 'Mixer' = 'mixer'; 'Second start' = 'second'; 'Third start' = 'third'; 'Hidden start' = 'hidden';
    'Start while hidden' = 'hidden2'; 'taskkill run' = 'taskkill'; 'Tray quit run' = 'trayquit'; 'Last run' = 'mixer2'; 'Before the taskbar' = 'login' }
foreach ($k in $logs.Keys) {
    Write-Output "`n---- $k stderr ----"
    Get-Content "$work\$($logs[$k]).err" -ErrorAction SilentlyContinue
}
if ($script:failures -gt 0) {
    foreach ($k in $logs.Keys) {
        $n = $logs[$k]
        Annotate 'warning' "$k stderr" ((Get-Content -Raw "$work\$n.err" -ErrorAction SilentlyContinue) + (Get-Content -Raw "$work\$n.out" -ErrorAction SilentlyContinue))
    }
    $procs = Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowTitle } | ForEach-Object { "$($_.ProcessName) [$($_.Id)]: $($_.MainWindowTitle)" }
    Annotate 'warning' 'Windows with a title' ($procs -join "`n")
}
Get-Process bananasplit -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
if ($savedRun) { New-ItemProperty -Path $run -Name BananaSplit -Value $savedRun -PropertyType String -Force | Out-Null }
else { Remove-ItemProperty -Path $run -Name BananaSplit -ErrorAction SilentlyContinue }

Write-Output "`n$($script:checks - $script:failures) of $($script:checks) checks passed"
Annotate $(if ($script:failures -gt 0) { 'error' } else { 'notice' }) 'Windows launch' "$($script:checks - $script:failures) of $($script:checks) checks passed"
if ($script:failures -gt 0) { exit 1 }
