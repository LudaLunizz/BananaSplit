# Launches the real bananasplit.exe the way a user does and checks that it
# works: a double-click start opens the mixer and its desktop window, a
# second start does not start a second mixer, and --quit saves and closes
# everything. Run on a Windows desktop session (CI: windows-latest).
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
try { $state = Get-Http '/api/state' | ConvertFrom-Json } catch { }
Check ($null -ne $state) "its state is valid JSON"
if ($state) {
    $line = "state: rate={0} block={1} cables={2} status='{3}' channels={4}" -f $state.sr, $state.bs, $state.cables, $state.status, @($state.ch).Count
    Write-Output $line
    Annotate 'notice' 'Mixer state' $line
}
Check (Until { (Windows-Titled 'BananaSplit').Count -ge 1 } 30) "a window titled BananaSplit opens"
Check (Until { @(Get-NetTCPConnection -LocalPort $Port -State Established -ErrorAction SilentlyContinue).Count -ge 1 } 30) "the window's page connects to the mixer"
Start-Sleep -Seconds 3
Screenshot 'launch'
Check (-not $p.HasExited) "the mixer keeps running"

# ---- starting it again brings up the running one ----
$before = (Mixers).Count
$p2 = Start-Process -FilePath $Exe -PassThru -RedirectStandardError "$work\second.err" -RedirectStandardOutput "$work\second.out"
Check ($p2.WaitForExit(15000) -and $p2.ExitCode -eq 0) "a second start hands over to the running mixer and exits"
Check ((Get-Http '/api/ping') -eq 'bananasplit-gui' -and -not $p.HasExited) "the first mixer is still the one running"

# ---- quitting saves and closes everything ----
& $Exe --quit
Check ($LASTEXITCODE -eq 0) "--quit is accepted"
Check ($p.WaitForExit(15000)) "the mixer exits after --quit"
Check (Until { (Mixers).Count -eq 0 } 15) "no BananaSplit process is left (window closed too)"
Check (Test-Path "$work\Roaming\bananasplit\bananasplit.ini") "the settings were saved"
if ($p.HasExited) { Check ($p.ExitCode -eq 0) "the mixer exit code is 0 (was $($p.ExitCode))" }

Write-Output "`n---- mixer stderr ----"
Get-Content "$work\mixer.err" -ErrorAction SilentlyContinue
Write-Output "---- second start stderr ----"
Get-Content "$work\second.err" -ErrorAction SilentlyContinue
if ($script:failures -gt 0) {
    Annotate 'warning' 'Mixer stderr' ((Get-Content -Raw "$work\mixer.err" -ErrorAction SilentlyContinue) + (Get-Content -Raw "$work\mixer.out" -ErrorAction SilentlyContinue))
    Annotate 'warning' 'Second start stderr' ((Get-Content -Raw "$work\second.err" -ErrorAction SilentlyContinue) + (Get-Content -Raw "$work\second.out" -ErrorAction SilentlyContinue))
    $procs = Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowTitle } | ForEach-Object { "$($_.ProcessName) [$($_.Id)]: $($_.MainWindowTitle)" }
    Annotate 'warning' 'Windows with a title' ($procs -join "`n")
}
Get-Process bananasplit -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue

Write-Output "`n$($script:checks - $script:failures) of $($script:checks) checks passed"
Annotate $(if ($script:failures -gt 0) { 'error' } else { 'notice' }) 'Windows launch' "$($script:checks - $script:failures) of $($script:checks) checks passed"
if ($script:failures -gt 0) { exit 1 }
