# Launches the real bananasplit.exe the way a user does and checks that it
# works: a double-click start opens the mixer and its desktop window; a
# second start brings that window up instead of starting a second mixer or
# window; the close button minimizes the window (BananaSplit keeps running);
# --quit saves and closes everything, the window too; and with "Closing the
# window keeps it running" off, closing the window quits. Run on a Windows
# desktop session (CI: windows-latest).
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

# ---- the command line still works ----
$v = (& $Exe --version 2>&1 | Out-String)
Check ($LASTEXITCODE -eq 0 -and $v -match 'BananaSplit \d+\.\d+\.\d+') "--version prints the version ($($v.Trim()))"
$d = (& $Exe --print-default-config 2>&1 | Out-String)
Check ($LASTEXITCODE -eq 0 -and $d -match '\[strip\.1\]|\[strip1\]|\[global\]') "--print-default-config prints a config"
$l = (& $Exe --list-devices 2>&1 | Out-String)
Write-Output "--list-devices (exit $LASTEXITCODE):"; Write-Output $l
Annotate 'notice' "--list-devices (exit $LASTEXITCODE)" $l

# ---- a double-click start: no arguments ----
$p = Start-Process -FilePath $Exe -PassThru -RedirectStandardError "$work\mixer.err" -RedirectStandardOutput "$work\mixer.out"
Check (Until { (Get-Http '/api/ping') -eq 'bananasplit-gui' }) "the mixer answers on $url"
$state = $null
Check (Until { $script:state = Get-Http '/api/state' | ConvertFrom-Json; $null -ne $script:state.ch }) "it publishes its state (valid JSON, with the channels)"
if ($state) {
    $line = "state: rate={0} block={1} cables={2} status='{3}' channels={4}" -f $state.sr, $state.bs, $state.cables, $state.status, @($state.ch).Count
    Write-Output $line
    Annotate 'notice' 'Mixer state' $line
}
Check (Until { (App-Windows).Count -ge 1 } 30) "a window titled BananaSplit opens"
Check (Until { @(Get-NetTCPConnection -LocalPort $Port -State Established -ErrorAction SilentlyContinue).Count -ge 1 } 30) "the window's page connects to the mixer"
Start-Sleep -Seconds 3
Screenshot 'launch'
Check (-not $p.HasExited) "the mixer keeps running"

# ---- starting it again brings up the running one ----
$p2 = Start-Process -FilePath $Exe -PassThru -RedirectStandardError "$work\second.err" -RedirectStandardOutput "$work\second.out"
Check ($p2.WaitForExit(15000) -and $p2.ExitCode -eq 0) "a second start hands over to the running mixer and exits"
Check ((Get-Http '/api/ping') -eq 'bananasplit-gui' -and -not $p.HasExited) "the first mixer is still the one running"
Start-Sleep -Seconds 2
Check ((App-Windows).Count -eq 1) "it brought up the open window instead of opening another"

# ---- the close button keeps BananaSplit running ----
$before = Window-State
$posted = Close-Window
$minimized = Until { Window-Minimized } 10
Check $minimized "closing the window minimizes it"
if (-not $minimized) { Annotate 'warning' 'Window state' "before: $before`n$posted`nafter: $(Window-State)" }
Start-Sleep -Seconds 1
Check (-not $p.HasExited -and (Mixers).Count -ge 2) "and the mixer and its window keep running"
$p3 = Start-Process -FilePath $Exe -PassThru -RedirectStandardError "$work\third.err" -RedirectStandardOutput "$work\third.out"
Check ($p3.WaitForExit(15000) -and $p3.ExitCode -eq 0) "starting BananaSplit again"
$back = Until { (App-Windows).Count -eq 1 -and -not (Window-Minimized) } 10
Check $back "brings the window back"
if (-not $back) { Annotate 'warning' 'Window state' (Window-State) }
Screenshot 'restored'

# ---- quitting saves and closes everything ----
& $Exe --quit
Check ($LASTEXITCODE -eq 0) "--quit is accepted"
Check ($p.WaitForExit(15000)) "the mixer exits after --quit"
Check (Until { (Mixers).Count -eq 0 } 15) "no BananaSplit process is left (window closed too)"
$ini = "$work\Roaming\bananasplit\bananasplit.ini"
Check (Test-Path $ini) "the settings were saved"
if ($p.HasExited) { Check ($p.ExitCode -eq 0) "the mixer exit code is 0 (was $($p.ExitCode))" }

# ---- with "Closing the window keeps it running" off, closing it quits ----
$p4 = Start-Process -FilePath $Exe -PassThru -RedirectStandardError "$work\mixer2.err" -RedirectStandardOutput "$work\mixer2.out"
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
Check ((Get-Content -Raw $ini -ErrorAction SilentlyContinue) -match 'close_to_tray = false') "the setting was saved"
if ($p4.HasExited) { Check ($p4.ExitCode -eq 0) "the mixer exit code is 0 (was $($p4.ExitCode))" }

$logs = [ordered]@{ 'Mixer' = 'mixer'; 'Second start' = 'second'; 'Third start' = 'third'; 'Second mixer run' = 'mixer2' }
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

Write-Output "`n$($script:checks - $script:failures) of $($script:checks) checks passed"
Annotate $(if ($script:failures -gt 0) { 'error' } else { 'notice' }) 'Windows launch' "$($script:checks - $script:failures) of $($script:checks) checks passed"
if ($script:failures -gt 0) { exit 1 }
