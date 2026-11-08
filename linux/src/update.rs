//! The updater: new versions of BananaSplit from its GitHub releases.
//!
//! * **Off by default.** With "Install updates automatically" off (the
//!   default, `[gui] auto_update = false`), BananaSplit never contacts
//!   anything on its own: it looks for a new version only when the user
//!   clicks "Check now" (Menu ▸ Updates) or runs `bananasplit --update`.
//! * **On:** the GUI checks a minute after it starts and then once a day,
//!   downloads and installs a new version by itself once it has been
//!   published for two days (Check now offers it at once), and says that a
//!   restart finishes the update. It never restarts on its own: that would
//!   cut the audio. Where installing needs an administrator (the .deb), it
//!   downloads and checks the package, then waits for a click (and the
//!   password).
//!
//! What counts as a release: a GitHub release of `LudaLunizz/BananaSplit`
//! that is published (not a draft) and not marked pre-release, with this
//! platform's file. The file's version is in its name
//! (`bananasplit-2.8.0-linux-x86_64.tar.gz`, `bananasplit_2.8.0_amd64.deb`,
//! `bananasplit-2.8.0-windows-x64.exe`), the same names the release
//! workflow gives them.
//!
//! What is checked before anything is replaced:
//! * the file comes from this repository's release downloads, over HTTPS
//!   (curl with `--proto =https`; wget's redirects are read and one to
//!   plain HTTP is refused), and has the size GitHub lists;
//! * the release's `SHA256SUMS.txt` is signed (`SHA256SUMS.txt.sig`,
//!   Ed25519) with BananaSplit's release key, which is never on GitHub: a
//!   release without that signature is refused, so someone who can publish
//!   releases there still cannot make one this updater installs;
//! * its SHA-256 matches that `SHA256SUMS.txt`, and GitHub's digest of the
//!   upload when GitHub gives one;
//! * the new program runs here and says it is that version
//!   (`--version`), before it takes the old one's place.
//!
//! How it is installed:
//! * Linux, a program in a folder of the user's (the portable package's
//!   `~/.local/bin`, an unpacked folder): the new binary is written next
//!   to it and renamed over it, which is atomic; the running mixer keeps
//!   the old one until it ends. BananaSplit's own app-menu copy is updated
//!   too.
//! * Linux, the .deb: `apt-get install --no-remove` of the downloaded
//!   package through `pkexec` (the desktop asks for the password), after it
//!   is copied to a folder only root writes and checked again there.
//! * Windows: the running `bananasplit.exe` is renamed to
//!   `bananasplit.exe.old` (Windows allows that, not overwriting it), the
//!   new one takes its name, and the next start removes the old one.
//! * Anything else (another package manager, an AppImage, a folder it
//!   cannot write to): it says so and links the release page.
//!
//! Downloads use the system's `curl` (on Windows the one in System32, part
//! of Windows 10 and 11) or `wget`, so BananaSplit itself still has no
//! dependencies and no TLS code of its own.

use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

// The platform-free half (release list, checksums, signatures, versions)
// is shared: `core/src/release.rs`.
pub use bananasplit::release::*;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Tests only, with [`SOURCE_ENV`]: seconds before the first automatic check.
pub const DELAY_ENV: &str = "BANANASPLIT_UPDATE_DELAY";
/// Set for a restarted BananaSplit: the process id of the one it replaces,
/// which it waits for (that one still holds the GUI port and the audio).
pub const WAIT_ENV: &str = "BANANASPLIT_WAIT_PID";

/// The first automatic check, after the start: the desktop's network and
/// the audio have settled by then.
const FIRST_CHECK: Duration = Duration::from_secs(60);
const RECHECK: Duration = Duration::from_secs(24 * 3600);
const RETRY: Duration = Duration::from_secs(3600);
/// The downloads folders of the GUI and of `bananasplit --update`: one each,
/// so neither cleans away what the other is downloading.
const GUI_CACHE: &str = "update";
const CLI_CACHE: &str = "update-cli";
const MAX_LIST: u64 = 4 << 20;

// ---------------------------------------------------------------------------
// Downloads (curl or wget)
// ---------------------------------------------------------------------------

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A command that never opens a console window (Windows) and gets none of
/// libjack's descriptors (Linux).
fn command(program: &Path) -> Command {
    crate::sys::cloexec_all();
    let mut c = Command::new(program);
    c.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0); // not stopped by a terminal's Ctrl-C
    }
    c
}

pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).filter(|d| d.is_absolute()).map(|d| d.join(name)).find(|p| p.is_file())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tool {
    Curl(PathBuf),
    #[cfg_attr(windows, allow(dead_code))]
    Wget(PathBuf),
}

/// curl, or else wget. On Windows: the curl in System32 (part of Windows
/// 10 1803 and later), not one a PATH entry may put first.
pub fn find_tool() -> Option<Tool> {
    #[cfg(windows)]
    {
        if let Some(root) = std::env::var_os("SystemRoot").map(PathBuf::from) {
            let c = root.join("System32").join("curl.exe");
            if c.is_file() {
                return Some(Tool::Curl(c));
            }
        }
        which("curl.exe").map(Tool::Curl)
    }
    #[cfg(unix)]
    {
        which("curl").map(Tool::Curl).or_else(|| which("wget").map(Tool::Wget))
    }
}

/// `curl -V`'s version (major, minor), asked once.
#[cfg(windows)]
fn curl_version(curl: &Path) -> Option<(u32, u32)> {
    static V: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        let mut c = command(curl);
        c.arg("-V");
        let never = AtomicBool::new(false);
        let (_, out, _) = run(c, Duration::from_secs(10), &never, &mut || Ok(())).ok()?;
        parse_curl_version(&out)
    })
}

/// "curl 8.4.0 (Windows) libcurl/8.4.0 …" -> (8, 4)
#[cfg_attr(unix, allow(dead_code))]
fn parse_curl_version(out: &str) -> Option<(u32, u32)> {
    let v = first_line(out).strip_prefix("curl ")?.split_whitespace().next()?;
    let mut it = v.split('.');
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

const NO_TOOL: &str = if cfg!(windows) {
    "Windows' curl.exe was not found (it is part of Windows 10 and 11)"
} else {
    "curl or wget is needed to download updates; install curl"
};

/// Run a child process to its end: `stop` ends it early, `limit` too;
/// `tick` runs every 100 ms (progress). Returns (exit code, stdout, stderr).
fn run(
    mut cmd: Command,
    limit: Duration,
    stop: &AtomicBool,
    tick: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(Option<i32>, String, String), String> {
    // A program file just written can be "busy" for a moment: a process
    // forked meanwhile by another thread holds it open for writing until it
    // calls exec (CI saw this in the tests). It frees up within milliseconds.
    let mut busy = 0;
    let mut child: Child = loop {
        match cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && busy < 20 => {
                busy += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            r => break r.map_err(|e| format!("cannot run {:?}: {e}", cmd.get_program()))?,
        }
    };
    let pipe = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(mut r) = r {
                let _ = r.by_ref().take(1 << 20).read_to_end(&mut s);
                let _ = std::io::copy(&mut r, &mut std::io::sink());
            }
            String::from_utf8_lossy(&s).into_owned()
        })
    };
    let out = pipe(child.stdout.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
    let err = pipe(child.stderr.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
    let t0 = Instant::now();
    let why = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let (o, e) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
                return Ok((status.code(), o, e));
            }
            Ok(None) => {}
            Err(e) => break format!("waiting for the download: {e}"),
        }
        if stop.load(SeqCst) {
            break "stopped".to_string();
        }
        if t0.elapsed() > limit {
            break "timed out".to_string();
        }
        if let Err(e) = tick() {
            break e;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = child.kill();
    let _ = child.wait();
    let _ = (out.join(), err.join());
    Err(why)
}

fn first_line(s: &str) -> &str {
    s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

fn curl_error(code: i32, stderr: &str) -> String {
    let what = match code {
        5 | 6 => "could not find the server (is this computer online?)",
        7 => "could not connect to the server",
        28 => "the connection timed out",
        35 | 51 | 53 | 54 | 58 | 59 | 60 | 66 | 77 | 80 | 83 | 90 | 91 => "the secure (TLS) connection failed",
        47 => "too many redirects",
        63 => "the file is larger than expected",
        _ => "",
    };
    let detail = first_line(stderr).trim_start_matches("curl: ");
    match (what.is_empty(), detail.is_empty()) {
        (false, _) => what.to_string(),
        (true, false) => format!("curl failed (exit {code}): {}", cut(detail, 200)),
        (true, true) => format!("curl failed (exit {code})"),
    }
}

/// Did a redirect in wget's `-S` output go to a plain `http:` address?
fn redirected_to_http(headers: &str) -> bool {
    headers.lines().any(|l| {
        let l = l.trim().to_ascii_lowercase();
        l.strip_prefix("location:").is_some_and(|v| v.trim_start().starts_with("http:"))
    })
}

/// Download `url` to `dest` (through `dest.part`, renamed when complete).
/// `max`: the most it may be. `json`: GitHub's API (its headers).
pub fn fetch(
    src: &Source,
    url: &str,
    dest: &Path,
    max: u64,
    json: bool,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), String> {
    let tool = find_tool().ok_or(NO_TOOL)?;
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let _ = std::fs::remove_file(&part);
    let _ = std::fs::remove_file(dest);
    let limit = if json { Duration::from_secs(60) } else { Duration::from_secs(30 * 60) };
    let agent = format!("BananaSplit/{VERSION} (updater; https://github.com/{REPO})");
    let proto = if src.https { "=https" } else { "=http" };
    let cmd = match &tool {
        Tool::Curl(p) => {
            let mut c = command(p);
            // -q first: the user's ~/.curlrc cannot change what this does
            c.args([
                "-q",
                "-sS",
                "-L",
                "--proto",
                proto,
                "--proto-redir",
                proto,
                "--max-redirs",
                "8",
                "--connect-timeout",
                "20",
            ]);
            c.args(["--max-time", &limit.as_secs().to_string(), "--max-filesize", &max.to_string(), "--retry", "2"]);
            c.arg("-H").arg(format!("User-Agent: {agent}"));
            if json {
                c.args(["-H", "Accept: application/vnd.github+json", "-H", "X-GitHub-Api-Version: 2022-11-28"]);
            }
            // Schannel: a network that blocks revocation lists (curl 7.70 and later)
            #[cfg(windows)]
            if curl_version(p).is_some_and(|v| v >= (7, 70)) {
                c.arg("--ssl-revoke-best-effort");
            }
            c.args(["-w", "%{http_code}", "-o"]).arg(&part).arg(url);
            c
        }
        Tool::Wget(p) => {
            let mut c = command(p);
            // -S: the server's headers on stderr, to check every redirect
            // (wget has no --proto-redir; its --https-only is for recursion)
            c.args(["--no-config", "-q", "-S", "--max-redirect=8", "--timeout=20", "--tries=3"]);
            c.arg(format!("--user-agent={agent}"));
            if json {
                c.args(["--header=Accept: application/vnd.github+json", "--header=X-GitHub-Api-Version: 2022-11-28"]);
            }
            c.arg("-O").arg(&part).arg(url);
            c
        }
    };
    let mut tick = || {
        let n = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        progress(n);
        if n > max { Err("the file is larger than expected".to_string()) } else { Ok(()) }
    };
    let result = run(cmd, limit, stop, &mut tick);
    let fail = |e: String| {
        let _ = std::fs::remove_file(&part);
        Err(e)
    };
    let (code, out, err) = match result {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    match (&tool, code) {
        (Tool::Curl(_), Some(0)) => {
            let http = out.trim();
            if http != "200" {
                let why = match http {
                    "403" | "429" if json => {
                        "GitHub refused the request (too many in a short time); try again in an hour".to_string()
                    }
                    "404" => "not found (404)".to_string(),
                    "000" | "" => "no answer from the server".to_string(),
                    h => format!("the server answered HTTP {h}"),
                };
                return fail(why);
            }
        }
        (Tool::Curl(_), Some(c)) => return fail(curl_error(c, &err)),
        (Tool::Wget(_), Some(0)) => {
            if src.https && redirected_to_http(&err) {
                return fail("a redirect left HTTPS, so the download is not used".into());
            }
        }
        (Tool::Wget(_), Some(8)) => return fail("the server answered with an error".into()),
        (Tool::Wget(_), Some(c)) => {
            // wget's own message, not the server's headers (-S, indented)
            let d = err.lines().filter(|l| !l.starts_with("  ")).map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
            return fail(if d.is_empty() {
                format!("wget failed (exit {c})")
            } else {
                format!("wget failed (exit {c}): {}", cut(d, 200))
            });
        }
        (_, None) => return fail("the download was stopped".into()),
    }
    let n = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if n > max {
        return fail("the file is larger than expected".into());
    }
    progress(n);
    std::fs::rename(&part, dest).or_else(|e| fail(format!("{}: {e}", dest.display())))
}

// ---------------------------------------------------------------------------
// This installation
// ---------------------------------------------------------------------------

/// Where downloads are kept until they are installed (`sub`: the GUI's or
/// the command line's own folder).
pub fn cache_dir(sub: &str) -> Option<PathBuf> {
    #[cfg(unix)]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".cache")));
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).filter(|p| p.is_absolute());
    base.map(|b| b.join("bananasplit").join(sub))
}

fn make_cache(sub: &str) -> Result<PathBuf, String> {
    let d = cache_dir(sub).ok_or("no folder for downloads (no home folder)")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&d)
            .map_err(|e| format!("{}: {e}", d.display()))?;
    }
    #[cfg(windows)]
    std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    Ok(d)
}

/// How this copy of BananaSplit can be updated.
#[derive(Debug, Clone, PartialEq)]
pub enum Method {
    /// Write the new program over these files (the running one first).
    Replace(Vec<PathBuf>),
    /// `pkexec apt-get install` of the new .deb.
    Deb,
    /// Not by itself: why (the user downloads it).
    Manual(String),
}

impl Method {
    pub fn package(&self) -> Package {
        if cfg!(windows) {
            return Package::WindowsExe;
        }
        let deb = match self {
            Method::Deb => true,
            Method::Replace(_) => false,
            // e.g. the .deb without apt-get: its version is the .deb's
            Method::Manual(_) => self_path().is_some_and(|p| deb_owned(&p)),
        };
        if deb { Package::Deb } else { Package::Tarball }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Method::Replace(_) => "replace",
            Method::Deb => "package",
            Method::Manual(_) => "manual",
        }
    }
}

const DEB_PROGRAM: &str = "/usr/bin/bananasplit";
#[cfg(unix)]
const APT_GET: &str = "/usr/bin/apt-get";

/// `/usr/bin/bananasplit`, installed by the bananasplit .deb.
fn deb_owned(exe: &Path) -> bool {
    exe == Path::new(DEB_PROGRAM)
        && ["/var/lib/dpkg/info/bananasplit.list", "/var/lib/dpkg/info/bananasplit:amd64.list"]
            .iter()
            .any(|l| std::fs::read_to_string(l).is_ok_and(|t| t.lines().any(|x| x == DEB_PROGRAM)))
}

/// The program file that runs (on Linux after an update replaced it: the
/// file now at its place).
fn self_path() -> Option<PathBuf> {
    crate::desktop::program_path()
}

/// Can this process create files in `dir`?
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".bananasplit-write-test-{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(f) => {
            drop(f);
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

pub fn method() -> Method {
    if !cfg!(target_arch = "x86_64") {
        return Method::Manual("the release page has BananaSplit for x86-64 (64-bit Intel and AMD) only".into());
    }
    #[cfg(unix)]
    if crate::desktop::flatpak_id().is_some() {
        return Method::Manual("this BananaSplit is a Flatpak: install the new .flatpak from the release page".into());
    }
    #[cfg(unix)]
    if std::env::var_os("APPIMAGE").is_some_and(|a| !a.is_empty()) {
        return Method::Manual(
            "this BananaSplit runs from an AppImage: download the new one from the release page".into(),
        );
    }
    let Some(exe) = self_path() else { return Method::Manual("cannot find this program's file".into()) };
    #[cfg(unix)]
    {
        if deb_owned(&exe) {
            if Path::new(APT_GET).is_file() {
                return Method::Deb;
            }
            return Method::Manual("BananaSplit was installed from its .deb, and apt-get is not here: install the new .deb from the release page".into());
        }
        if crate::desktop::is_system_path(&exe) {
            return Method::Manual(format!(
                "{} was installed by a package manager: update BananaSplit there",
                exe.display()
            ));
        }
    }
    #[cfg_attr(windows, allow(unused_mut))]
    let mut targets = vec![exe];
    // The copy BananaSplit's own app-menu entry starts (`--install`), when
    // this one is another (run from the unpacked package).
    #[cfg(unix)]
    if let Some(menu) = crate::desktop::installed_program()
        && !same_file(&menu, &targets[0])
    {
        targets.push(menu);
    }
    for t in &targets {
        let dir = t.parent().unwrap_or(Path::new("."));
        if !writable(dir) {
            return Method::Manual(format!(
                "BananaSplit cannot write to {}: download the new version from the release page",
                dir.display()
            ));
        }
    }
    Method::Replace(targets)
}

// ---------------------------------------------------------------------------
// Download, check, install
// ---------------------------------------------------------------------------

/// The newest release for this computer, if any.
pub fn find(src: &Source, cache: &str, pkg: Package, stop: &AtomicBool) -> Result<Option<Offer>, String> {
    let dir = make_cache(cache)?;
    let file = dir.join("releases.json");
    fetch(src, &src.api, &file, MAX_LIST, true, stop, &mut |_| {})?;
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()));
    let _ = std::fs::remove_file(&file);
    let json = Json::parse(&text?).map_err(|e| format!("GitHub's answer could not be read ({e})"))?;
    pick(&json, pkg, src)
}

/// Check a downloaded file against everything the release says about it.
pub fn verify(file: &Path, offer: &Offer, sums: Option<&str>) -> Result<[u8; 32], String> {
    let name = &offer.asset.name;
    let len = std::fs::metadata(file).map_err(|e| format!("{}: {e}", file.display()))?.len();
    if len != offer.asset.size {
        return Err(format!("{name} has {len} bytes, the release says {}", offer.asset.size));
    }
    let listed = match sums {
        Some(text) => {
            Some(sum_for(text, name).ok_or_else(|| format!("the release's SHA256SUMS.txt does not list {name}"))?)
        }
        None => None,
    };
    if offer.asset.digest.is_none() && listed.is_none() {
        return Err(format!("the release has no checksum for {name}, so it is not installed"));
    }
    let got = sha256_file(file)?;
    for (want, from) in [(offer.asset.digest, "GitHub's checksum"), (listed, "SHA256SUMS.txt")] {
        if want.is_some_and(|w| w != got) {
            return Err(format!(
                "{name} does not match {from} (damaged or not the released file); nothing was changed"
            ));
        }
    }
    Ok(got)
}

/// A small text file of the release (its checksums, their signature).
fn fetch_text(src: &Source, dir: &Path, a: &Asset, max: u64, stop: &AtomicBool) -> Result<String, String> {
    let f = dir.join(&a.name);
    fetch(src, &a.url, &f, max, false, stop, &mut |_| {}).map_err(|e| format!("downloading {}: {e}", a.name))?;
    let t = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()));
    let _ = std::fs::remove_file(&f);
    t
}

/// Download the release file and check it. Returns its path and SHA-256.
pub fn download(
    src: &Source,
    cache: &str,
    offer: &Offer,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<(PathBuf, [u8; 32]), String> {
    let (Some(sums), Some(sig)) = (&offer.sums, &offer.sig) else {
        return Err(
            "the release is not signed (it has no SHA256SUMS.txt and SHA256SUMS.txt.sig), so it is not installed"
                .into(),
        );
    };
    let dir = make_cache(cache)?;
    clean_cache(&dir);
    let sums = fetch_text(src, &dir, sums, MAX_SUMS, stop)?;
    let sig = fetch_text(src, &dir, sig, MAX_SIG, stop)?;
    // before the big download: an unsigned release is not worth fetching
    check_signature(&sums, &sig, &src.key)?;
    let file = dir.join(&offer.asset.name);
    fetch(src, &offer.asset.url, &file, offer.asset.size, false, stop, progress)
        .map_err(|e| format!("downloading {}: {e}", offer.asset.name))?;
    match verify(&file, offer, Some(&sums)) {
        Ok(hash) => Ok((file, hash)),
        Err(e) => {
            let _ = std::fs::remove_file(&file);
            Err(e)
        }
    }
}

/// Remove what earlier downloads left in the downloads folder.
fn clean_cache(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        let ours = name.starts_with("bananasplit")
            || name.starts_with("SHA256SUMS.txt")
            || name.starts_with("releases.json")
            || name == "unpacked";
        if !ours {
            continue;
        }
        if p.is_dir() && !p.is_symlink() {
            let _ = std::fs::remove_dir_all(&p);
        } else {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Does `program --version` say it is BananaSplit `version`?
pub fn check_program(program: &Path, version: &str) -> Result<(), String> {
    let mut c = command(program);
    c.arg("--version");
    let never = AtomicBool::new(false);
    let (code, out, err) = run(c, Duration::from_secs(20), &never, &mut || Ok(()))
        .map_err(|e| format!("the new version did not run: {e}"))?;
    let line = first_line(&out);
    let said = line.strip_prefix("BananaSplit ").and_then(|r| r.split_whitespace().next());
    match said {
        Some(v) if code == Some(0) && v == version => Ok(()),
        Some(v) if code == Some(0) => {
            Err(format!("the downloaded program says it is BananaSplit {}, not {version}", cut(v, 40)))
        }
        _ => {
            let what = if line.is_empty() { first_line(&err) } else { line };
            Err(format!(
                "the new version does not run on this computer ({})",
                if what.is_empty() { "no answer".to_string() } else { cut(what, 200) }
            ))
        }
    }
}

/// Linux: the program from the portable package (`NAME/bananasplit`).
#[cfg(unix)]
fn unpack(tgz: &Path, version: &str) -> Result<PathBuf, String> {
    let tar = which("tar").ok_or("tar is needed to unpack the update")?;
    let member = format!("bananasplit-{version}-linux-x86_64/bananasplit");
    let dir = tgz.parent().unwrap_or(Path::new(".")).join("unpacked");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let never = AtomicBool::new(false);
    let mut c = command(&tar);
    c.arg("-xzf").arg(tgz).arg("-C").arg(&dir).arg("--no-same-owner").arg(&member);
    match run(c, Duration::from_secs(120), &never, &mut || Ok(())) {
        Ok((Some(0), _, _)) => {}
        Ok((_, _, err)) => return Err(format!("could not unpack the update: {}", cut(first_line(&err), 200))),
        Err(e) => return Err(format!("could not unpack the update: {e}")),
    }
    let bin = dir.join(&member);
    let meta = std::fs::symlink_metadata(&bin).map_err(|_| format!("the package has no {member}"))?;
    if !meta.file_type().is_file() || meta.len() == 0 {
        return Err(format!("{member} in the package is not a program file"));
    }
    Ok(bin)
}

/// Windows: where the running program was moved by an update (helper
/// processes are started from it: they must be the same version).
#[cfg(windows)]
static MOVED: Mutex<Option<PathBuf>> = Mutex::new(None);
/// Windows: the program's own path, where the new version now is (Windows
/// may name the running program by the file it was moved to).
#[cfg(windows)]
static ORIGINAL: Mutex<Option<PathBuf>> = Mutex::new(None);

#[cfg(windows)]
pub fn moved_program() -> Option<PathBuf> {
    lock(&MOVED).clone()
}

#[cfg(windows)]
pub fn original_program() -> Option<PathBuf> {
    lock(&ORIGINAL).clone()
}

fn retry<T>(mut f: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    // A virus scanner may hold a new file for a moment.
    let mut tries = 0;
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if tries < 15 && e.kind() != std::io::ErrorKind::NotFound => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(e),
        }
    }
}

/// The permissions of the new program: those of the one it replaces (no
/// set-id bits), runnable by whoever could read it, always by its owner.
#[cfg(unix)]
fn program_mode(old: Option<u32>) -> u32 {
    let m = old.map_or(0o755, |m| m & 0o777);
    m | ((m & 0o444) >> 2) | 0o700
}

/// The new program file next to `target`, before it takes its place.
fn staging_name(target: &Path) -> PathBuf {
    let dir = target.parent().unwrap_or(Path::new("."));
    if cfg!(windows) {
        dir.join(format!("bananasplit.update-{}.exe", std::process::id()))
    } else {
        let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "bananasplit".into());
        dir.join(format!(".{name}.update-{}", std::process::id()))
    }
}

/// Held while program files are renamed. Quitting sets the updater's
/// `stop` and then takes this lock: renaming that has begun ends first, and
/// renaming that has not begun sees `stop` and changes nothing. So the
/// program is never left without its file (Windows renames twice), however
/// long the check of the new program took.
static RENAMING: Mutex<()> = Mutex::new(());

/// Put `new` (a checked BananaSplit `version`) in the place of each target.
/// The first target is the running program; nothing is changed unless the
/// new program runs here. A later target (the app menu's copy) that cannot
/// be replaced is a warning: the running program is new by then.
/// `stop`: BananaSplit is quitting (nothing more is renamed).
pub fn replace(new: &Path, targets: &[PathBuf], version: &str, stop: &AtomicBool) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    for (k, target) in targets.iter().enumerate() {
        let result = replace_one(new, target, version, k == 0, stop);
        match result {
            Ok(()) => {}
            Err(e) if k == 0 => return Err(e),
            Err(e) => warnings.push(format!("{} was not updated ({e})", target.display())),
        }
    }
    Ok(warnings)
}

fn replace_one(new: &Path, target: &Path, version: &str, first: bool, stop: &AtomicBool) -> Result<(), String> {
    {
        let k = usize::from(!first);
        let stage = staging_name(target);
        let _ = std::fs::remove_file(&stage);
        let staged = (|| -> Result<(), String> {
            std::fs::copy(new, &stage).map_err(|e| format!("{}: {e}", stage.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let was = std::fs::metadata(target).map(|m| m.permissions().mode()).ok();
                std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(program_mode(was)))
                    .map_err(|e| format!("{}: {e}", stage.display()))?;
            }
            // to disk before it takes the old one's place (Windows flushes
            // only a handle opened for writing)
            std::fs::OpenOptions::new()
                .write(true)
                .open(&stage)
                .and_then(|f| f.sync_all())
                .map_err(|e| format!("{}: {e}", stage.display()))?;
            if k == 0 {
                check_program(&stage, version)?;
            }
            Ok(())
        })();
        if let Err(e) = staged {
            let _ = std::fs::remove_file(&stage);
            return Err(e);
        }
        let _renaming = lock(&RENAMING);
        if stop.load(SeqCst) {
            let _ = std::fs::remove_file(&stage);
            return Err("BananaSplit is quitting: nothing was changed".into());
        }
        #[cfg(unix)]
        if let Err(e) = retry(|| std::fs::rename(&stage, target)) {
            let _ = std::fs::remove_file(&stage);
            return Err(format!("{}: {e}", target.display()));
        }
        #[cfg(windows)]
        {
            // A running program cannot be overwritten, but it can be renamed.
            let mut old = PathBuf::from(format!("{}.old", target.display()));
            if old.exists() && std::fs::remove_file(&old).is_err() {
                old = PathBuf::from(format!("{}.old-{}", target.display(), std::process::id()));
            }
            if let Err(e) = retry(|| std::fs::rename(target, &old)) {
                let _ = std::fs::remove_file(&stage);
                return Err(format!("{}: {e}", target.display()));
            }
            if let Err(e) = retry(|| std::fs::rename(&stage, target)) {
                let _ = std::fs::rename(&old, target);
                let _ = std::fs::remove_file(&stage);
                return Err(format!("{}: {e}", target.display()));
            }
            if k == 0 {
                // The first update of this session moved the program that
                // runs; a second one moves the first one's file, not this.
                lock(&MOVED).get_or_insert(old);
                lock(&ORIGINAL).get_or_insert(target.to_path_buf());
            }
        }
    }
    Ok(())
}

/// Is `f`, in the program's folder, something an update left there? The
/// old Windows program (`bananasplit.exe.old`, in use until the old
/// BananaSplit ended) or an unfinished new file of an update that is not
/// running any more (not this process's, not a live one's).
fn stale(windows: bool, exe: &str, f: &str, me: u32, alive: &dyn Fn(&str) -> bool) -> bool {
    let pid = |p: &str| {
        !p.is_empty() && p.len() <= 10 && p.bytes().all(|b| b.is_ascii_digit()) && p != me.to_string() && !alive(p)
    };
    if windows {
        f == format!("{exe}.old")
            || f.strip_prefix(&format!("{exe}.old-"))
                .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            || f.strip_prefix("bananasplit.update-").and_then(|r| r.strip_suffix(".exe")).is_some_and(pid)
    } else {
        f.strip_prefix(&format!(".{exe}.update-")).is_some_and(pid)
    }
}

/// Remove what an update left next to the program (see [`stale`]).
pub fn clean_leftovers() {
    let Some(exe) = self_path() else { return };
    let Some(dir) = exe.parent() else { return };
    let Some(name) = exe.file_name().map(|n| n.to_string_lossy().into_owned()) else { return };
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let f = e.file_name().to_string_lossy().into_owned();
        if stale(cfg!(windows), &name, &f, std::process::id(), &pid_alive) && e.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// What runs as root (`sh -c`, with the .deb, its SHA-256 and apt-get):
/// the checked .deb is in a folder of the user's, so it is copied to a
/// folder only root can write and checked again there before apt-get gets
/// it. `--no-remove`: never remove another package to install this one.
#[cfg(unix)]
const ROOT_INSTALL: &str = r#"set -e
d=$(mktemp -d /tmp/bananasplit-update.XXXXXX)
trap 'rm -rf "$d"' EXIT
cp -- "$1" "$d/bananasplit.deb"
set -- "$(sha256sum < "$d/bananasplit.deb")" "$2" "$3"
if [ "${1%% *}" != "$2" ]; then echo "the package changed after it was checked" >&2; exit 3; fi
"$3" install -y --no-remove "$d/bananasplit.deb""#;

/// Linux: install the new .deb. From the app, the desktop asks for the
/// password; from a terminal (`interactive`), pkexec asks in it.
#[cfg(unix)]
fn install_deb(deb: &Path, hash: &[u8; 32], version: &str, stop: &AtomicBool, interactive: bool) -> Result<(), String> {
    let Some(pkexec) = which("pkexec") else {
        return Err(format!(
            "installing a package needs pkexec, which is not here. Install it in a terminal:  sudo apt install {}",
            deb.display()
        ));
    };
    install_deb_with(&pkexec, Path::new(APT_GET), Path::new(DEB_PROGRAM), deb, hash, version, stop, interactive)
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn install_deb_with(
    pkexec: &Path,
    apt: &Path,
    program: &Path,
    deb: &Path,
    hash: &[u8; 32],
    version: &str,
    stop: &AtomicBool,
    interactive: bool,
) -> Result<(), String> {
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    let args = |c: &mut Command| {
        c.arg("/bin/sh").arg("-c").arg(ROOT_INSTALL).arg("bananasplit-update").arg(deb).arg(&hex).arg(apt);
    };
    let result = if interactive {
        // In the terminal's foreground, with its input and output: pkexec's
        // text agent reads the password there.
        crate::sys::cloexec_all();
        let mut c = std::process::Command::new(pkexec);
        args(&mut c);
        c.status()
            .map(|s| (s.code(), String::new(), String::new()))
            .map_err(|e| format!("cannot run {}: {e}", pkexec.display()))
    } else {
        let mut c = command(pkexec);
        args(&mut c);
        // No time limit: the password dialog waits for the user.
        run(c, Duration::from_secs(24 * 3600), stop, &mut || Ok(()))
    };
    match result {
        Ok((Some(0), _, _)) => {}
        Ok((Some(3), _, _)) => return Err("the package changed after it was checked; nothing was installed".into()),
        Ok((Some(126), _, _)) => return Err("installing was cancelled (no password given)".into()),
        Ok((Some(127), _, err)) => {
            let why = first_line(&err);
            return Err(format!(
                "could not get permission to install the package{}",
                if why.is_empty() { String::new() } else { format!(" ({})", cut(why, 200)) }
            ));
        }
        Ok((code, _, err)) => {
            let last = err.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("");
            return Err(format!(
                "apt-get failed (exit {}): {}",
                code.map_or("?".into(), |c| c.to_string()),
                cut(last, 200)
            ));
        }
        Err(e) => return Err(e),
    }
    check_program(program, version)
}

/// Install a downloaded, checked release file. Returns where it went, and
/// warnings (a copy that could not be updated).
pub fn install(
    method: &Method,
    offer: &Offer,
    file: &Path,
    hash: &[u8; 32],
    stop: &AtomicBool,
    interactive: bool,
) -> Result<(String, Vec<String>), String> {
    let _ = (hash, stop, interactive);
    match method {
        Method::Manual(why) => Err(why.clone()),
        Method::Deb => {
            #[cfg(unix)]
            {
                install_deb(file, hash, &offer.version, stop, interactive)?;
                Ok((DEB_PROGRAM.to_string(), Vec::new()))
            }
            #[cfg(windows)]
            Err("no packages on Windows".into())
        }
        Method::Replace(targets) => {
            #[cfg(unix)]
            let program = unpack(file, &offer.version)?;
            #[cfg(windows)]
            let program = file.to_path_buf();
            let r = replace(&program, targets, &offer.version, stop);
            #[cfg(unix)]
            if let Some(d) = program.parent().and_then(Path::parent) {
                let _ = std::fs::remove_dir_all(d); // the unpacked folder
            }
            let warnings = r?;
            Ok((targets.iter().map(|t| t.display().to_string()).collect::<Vec<_>>().join(", "), warnings))
        }
    }
}

// ---------------------------------------------------------------------------
// The updater of the GUI
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    /// Not checked yet.
    Idle,
    Checking,
    /// This is the newest version.
    Current,
    /// A newer version: `offer`.
    Available,
    Downloading,
    /// The .deb is downloaded and checked; installing it needs a click.
    Prepared,
    Installing,
    /// Installed: restart BananaSplit to use it.
    Ready,
    Failed,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Checking => "checking",
            Phase::Current => "current",
            Phase::Available => "available",
            Phase::Downloading => "downloading",
            Phase::Prepared => "prepared",
            Phase::Installing => "installing",
            Phase::Ready => "ready",
            Phase::Failed => "failed",
        }
    }

    fn busy(self) -> bool {
        matches!(self, Phase::Checking | Phase::Downloading | Phase::Installing)
    }
}

#[derive(Debug, Clone)]
pub struct Status {
    pub phase: Phase,
    pub offer: Option<Offer>,
    pub message: String,
    pub got: u64,
    pub total: u64,
    pub checked: Option<Instant>,
    /// The version this session installed (it runs after a restart).
    pub installed: Option<String>,
    pub method: Method,
    /// The .deb, downloaded and checked, and its SHA-256 then.
    pub prepared: Option<PathBuf>,
    pub prepared_hash: Option<[u8; 32]>,
    /// Bumped on every change but progress (the page fetches the notes).
    pub rev: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Job {
    Check { auto: bool },
    Install { auto: bool },
}

struct Inner {
    st: Status,
    job: Option<Job>,
    next_auto: Option<Instant>,
    /// Something the session line should say (an automatic update).
    event: Option<String>,
    /// Automatic installs of this version that failed: after two, it is
    /// left to the user (no new download every hour).
    auto_fails: (String, u32),
    done: bool,
}

pub struct Updater {
    inner: Mutex<Inner>,
    cv: Condvar,
    stop: AtomicBool,
    src: Source,
    started: Instant,
    first: Duration,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Updater {
    pub fn start() -> Arc<Updater> {
        let src = Source::from_env();
        let first = if src.is_test() {
            std::env::var(DELAY_ENV).ok().and_then(|s| s.parse().ok()).map_or(FIRST_CHECK, Duration::from_secs_f64)
        } else {
            FIRST_CHECK
        };
        let u = Arc::new(Updater {
            inner: Mutex::new(Inner {
                st: Status {
                    phase: Phase::Idle,
                    offer: None,
                    message: String::new(),
                    got: 0,
                    total: 0,
                    checked: None,
                    installed: None,
                    method: Method::Manual(String::new()),
                    prepared: None,
                    prepared_hash: None,
                    rev: 0,
                },
                job: None,
                next_auto: None,
                event: None,
                auto_fails: (String::new(), 0),
                done: false,
            }),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
            src,
            started: Instant::now(),
            first,
        });
        let w = u.clone();
        let spawned = std::thread::Builder::new().name("bs-update".into()).spawn(move || w.work());
        if spawned.is_err() {
            lock(&u.inner).done = true;
        }
        u
    }

    pub fn status(&self) -> Status {
        lock(&self.inner).st.clone()
    }

    fn set(&self, f: impl FnOnce(&mut Status)) {
        let mut g = lock(&self.inner);
        f(&mut g.st);
        g.st.rev += 1;
    }

    fn queue(&self, job: Job) -> Result<(), String> {
        let mut g = lock(&self.inner);
        if g.done {
            return Err("the updater is not running".into());
        }
        if g.st.phase.busy() || g.job.is_some() {
            return Err("already busy with an update".into());
        }
        g.job = Some(job);
        // busy from now on, also for the page's next frame
        g.st.phase = match job {
            Job::Check { .. } => Phase::Checking,
            Job::Install { .. } => Phase::Installing,
        };
        g.st.rev += 1;
        drop(g);
        self.cv.notify_all();
        Ok(())
    }

    /// "Check now".
    pub fn check(&self) -> Result<String, String> {
        self.queue(Job::Check { auto: false }).map(|()| "checking for updates".into())
    }

    /// "Install": the version found by the last check.
    pub fn install(&self) -> Result<String, String> {
        let st = self.status();
        match (&st.offer, st.phase) {
            (Some(o), Phase::Available | Phase::Prepared | Phase::Failed)
                if newer(&o.version, st.installed.as_deref().unwrap_or(VERSION)) =>
            {
                if let Method::Manual(why) = &st.method {
                    return Err(why.clone());
                }
                self.queue(Job::Install { auto: false }).map(|()| format!("installing BananaSplit {}", o.version))
            }
            (_, Phase::Ready) => Err("the update is installed: restart BananaSplit to use it".into()),
            _ => Err("there is no new version to install (check for updates first)".into()),
        }
    }

    /// A restart would start the version this session installed.
    pub fn ready(&self) -> bool {
        let g = lock(&self.inner);
        g.st.installed.is_some() && !g.st.phase.busy()
    }

    /// What the session line should say about an automatic update.
    pub fn take_event(&self) -> Option<String> {
        lock(&self.inner).event.take()
    }

    /// Called by the GUI's loop every few seconds: with automatic updates
    /// on, check a minute after the start and then once a day.
    pub fn tick(&self, auto: bool) {
        let mut g = lock(&self.inner);
        if !auto {
            g.next_auto = None;
            return;
        }
        let now = Instant::now();
        let next = *g
            .next_auto
            .get_or_insert_with(|| (self.started + self.first).max(now + self.first.min(Duration::from_secs(5))));
        if now < next || g.st.phase.busy() || g.job.is_some() || g.done {
            return;
        }
        g.next_auto = Some(now + RECHECK);
        g.job = Some(Job::Check { auto: true });
        drop(g);
        self.cv.notify_all();
    }

    pub fn shutdown(&self) {
        self.stop.store(true, SeqCst);
        let g = lock(&self.inner);
        self.cv.notify_all();
        // Not for long: a password dialog may be open (its apt-get goes on).
        let g = self.cv.wait_timeout_while(g, Duration::from_secs(3), |i| !i.done);
        drop(g);
        // But never in the middle of renaming the program's files: `stop` is
        // set, so once renaming that has begun is over, none begins.
        drop(lock(&RENAMING));
    }

    fn work(&self) {
        let m = method();
        self.set(|s| s.method = m);
        loop {
            let job = {
                let mut g = lock(&self.inner);
                loop {
                    if self.stop.load(SeqCst) {
                        g.done = true;
                        self.cv.notify_all();
                        return;
                    }
                    if let Some(j) = g.job {
                        break j;
                    }
                    g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
                }
            };
            // A panic here must not leave the updater "busy" for good.
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match job {
                Job::Check { auto } => self.do_check(auto),
                Job::Install { auto } => self.do_install(auto),
            }));
            let mut g = lock(&self.inner);
            if r.is_err() {
                g.st.phase = Phase::Failed;
                g.st.message = "the updater ran into an internal error".into();
                g.st.rev += 1;
            } else if g.st.phase.busy() {
                // every way out of a job says how it ended; never "busy" for good
                g.st.phase = Phase::Failed;
                g.st.message = "the update stopped before it finished".into();
                g.st.rev += 1;
            }
            if g.job == Some(job) {
                g.job = None;
            }
        }
    }

    fn do_check(&self, auto: bool) {
        let method = method();
        let pkg = method.package();
        self.set(|s| {
            s.phase = Phase::Checking;
            s.message = "checking for updates…".into();
            s.method = method.clone();
        });
        let found = find(&self.src, GUI_CACHE, pkg, &self.stop);
        let mut install = false;
        {
            let mut g = lock(&self.inner);
            let inner = &mut *g;
            let s = &mut inner.st;
            s.checked = Some(Instant::now());
            s.rev += 1;
            let have = s.installed.clone().unwrap_or_else(|| VERSION.to_string());
            match found {
                Err(e) => {
                    if s.installed.is_some() {
                        // what was installed still waits for the restart
                        s.phase = Phase::Ready;
                        s.message = format!(
                            "BananaSplit {have} is installed: restart BananaSplit to use it. (Checking again failed: {e})"
                        );
                    } else {
                        s.phase = Phase::Failed;
                        s.message = format!("Could not check for updates: {e}");
                    }
                    if auto {
                        inner.next_auto = Some(Instant::now() + RETRY);
                    }
                }
                Ok(Some(o)) if newer(&o.version, &have) => {
                    let keep = s.prepared.as_ref().is_some_and(|p| p.is_file())
                        && s.offer.as_ref().is_some_and(|p| p.version == o.version && p.asset == o.asset);
                    if !keep {
                        s.prepared = None;
                        s.prepared_hash = None;
                    }
                    s.message = match &method {
                        Method::Manual(why) => {
                            format!("BananaSplit {} is available. It cannot install itself here: {why}.", o.version)
                        }
                        _ if keep => format!(
                            "BananaSplit {} is downloaded and checked: install it now (asks for your password).",
                            o.version
                        ),
                        _ => format!("BananaSplit {} is available.", o.version),
                    };
                    s.phase = if keep { Phase::Prepared } else { Phase::Available };
                    let gave_up = inner.auto_fails.0 == o.version && inner.auto_fails.1 >= 2;
                    let young = settling(&o, now_utc());
                    if auto && !gave_up {
                        if matches!(method, Method::Manual(_)) || keep {
                            inner.event = Some(format!("BananaSplit {} is available: Menu ▸ Updates", o.version));
                        } else if let Some(wait) = young {
                            // installed by itself once it has been out for SETTLE
                            s.message.push_str(
                                " It installs itself once it has been out for two days; Install does it now.",
                            );
                            inner.event = Some(format!("BananaSplit {} is available: Menu ▸ Updates", o.version));
                            inner.next_auto = Some(Instant::now() + (wait + Duration::from_secs(60)).min(RECHECK));
                        } else {
                            install = true;
                        }
                    }
                    s.offer = Some(o);
                }
                Ok(o) => {
                    if s.installed.is_some() {
                        s.phase = Phase::Ready;
                        s.message = format!("BananaSplit {have} is installed: restart BananaSplit to use it.");
                    } else {
                        s.phase = Phase::Current;
                        s.message = format!("BananaSplit {VERSION} is the newest version.");
                    }
                    s.offer = o;
                    s.prepared = None;
                    s.prepared_hash = None;
                }
            }
        }
        if install {
            self.do_install(true);
        }
    }

    fn do_install(&self, auto: bool) {
        let st = self.status();
        let Some(offer) = st.offer.clone() else {
            // (a check in between found nothing newer)
            return self.set(|s| {
                s.phase = Phase::Failed;
                s.message = "there is no new version to install (check for updates first)".into();
            });
        };
        let method = st.method.clone();
        if let Method::Manual(why) = &method {
            return self.set(|s| {
                s.phase = Phase::Available;
                s.message = why.clone();
            });
        }
        let v = offer.version.clone();
        let fail = |e: String| {
            let mut g = lock(&self.inner);
            g.st.phase = Phase::Failed;
            g.st.message = format!("Updating to {v} failed: {e}");
            g.st.rev += 1;
            if auto {
                g.event = Some(format!("Automatic update to BananaSplit {v} failed: Menu ▸ Updates"));
                g.next_auto = Some(Instant::now() + RETRY);
                if g.auto_fails.0 == v {
                    g.auto_fails.1 += 1;
                } else {
                    g.auto_fails = (v.clone(), 1);
                }
            }
        };
        if method == Method::Deb && self.src.is_test() {
            // a package is installed as root: only ever from GitHub
            return fail("a package is installed only from GitHub, not from a test server".into());
        }
        // The .deb that was already downloaded and checked, still unchanged.
        let ready = st.prepared.clone().zip(st.prepared_hash).filter(|(p, h)| sha256_file(p).ok() == Some(*h));
        let (file, hash) = match ready {
            Some(f) => f,
            None => {
                self.set(|s| {
                    s.phase = Phase::Downloading;
                    s.got = 0;
                    s.total = offer.asset.size;
                    s.message = format!("downloading BananaSplit {v}…");
                    s.prepared = None;
                    s.prepared_hash = None;
                });
                let mut progress = |n: u64| lock(&self.inner).st.got = n.min(offer.asset.size);
                match download(&self.src, GUI_CACHE, &offer, &self.stop, &mut progress) {
                    Ok(f) => f,
                    Err(e) => return fail(e),
                }
            }
        };
        if method == Method::Deb && auto {
            let mut g = lock(&self.inner);
            g.st.phase = Phase::Prepared;
            g.st.prepared = Some(file);
            g.st.prepared_hash = Some(hash);
            g.st.message =
                format!("BananaSplit {v} is downloaded and checked: install it now (asks for your password).");
            g.st.rev += 1;
            g.event = Some(format!("BananaSplit {v} is ready to install: Menu ▸ Updates"));
            return;
        }
        self.set(|s| {
            s.phase = Phase::Installing;
            s.message = if method == Method::Deb {
                format!("installing BananaSplit {v} (enter your password)…")
            } else {
                format!("installing BananaSplit {v}…")
            };
        });
        let r = install(&method, &offer, &file, &hash, &self.stop, false);
        if r.is_err() && method == Method::Deb {
            // kept (checked) for another try, and for the command the message names
            let mut g = lock(&self.inner);
            g.st.prepared = Some(file.clone());
            g.st.prepared_hash = Some(hash);
        } else if let Some(d) = file.parent() {
            clean_cache(d);
        }
        match r {
            Ok((_, warnings)) => {
                let mut g = lock(&self.inner);
                g.st.phase = Phase::Ready;
                g.st.installed = Some(v.clone());
                g.st.prepared = None;
                g.st.prepared_hash = None;
                let extra =
                    if warnings.is_empty() { String::new() } else { format!(" Note: {}.", warnings.join("; ")) };
                g.st.message = format!("BananaSplit {v} is installed. Restart BananaSplit to use it.{extra}");
                g.st.rev += 1;
                if auto {
                    g.event =
                        Some(format!("BananaSplit {v} was installed: restart BananaSplit to use it (Menu ▸ Updates)"));
                }
            }
            Err(e) => fail(e),
        }
    }

    /// For the page's state (every frame): small.
    pub fn json(&self, o: &mut String, auto: bool) {
        let st = self.status();
        let _ = write!(o, "{{\"auto\":{auto},\"phase\":\"{}\",\"current\":", st.phase.as_str());
        crate::gui::jstr(o, VERSION);
        o.push_str(",\"latest\":");
        match &st.offer {
            Some(of) => crate::gui::jstr(o, &of.version),
            None => o.push_str("null"),
        }
        o.push_str(",\"installed\":");
        match &st.installed {
            Some(v) => crate::gui::jstr(o, v),
            None => o.push_str("null"),
        }
        o.push_str(",\"message\":");
        crate::gui::jstr(o, &st.message);
        let ago = st.checked.map(|c| c.elapsed().as_secs());
        let can = matches!(st.phase, Phase::Available | Phase::Prepared | Phase::Failed)
            && !matches!(st.method, Method::Manual(_))
            && st.offer.as_ref().is_some_and(|of| newer(&of.version, st.installed.as_deref().unwrap_or(VERSION)));
        let _ = write!(
            o,
            ",\"got\":{},\"total\":{},\"method\":\"{}\",\"canInstall\":{can},\"checkedAgo\":{},\"rev\":{}}}",
            st.got,
            st.total,
            st.method.as_str(),
            ago.map_or("null".into(), |a| a.to_string()),
            st.rev
        );
    }

    /// `GET /api/update`: the release's page and notes too.
    pub fn details_json(&self) -> String {
        let st = self.status();
        let mut o = String::from("{\"page\":");
        crate::gui::jstr(&mut o, st.offer.as_ref().map_or(RELEASES_PAGE, |of| of.page.as_str()));
        o.push_str(",\"notes\":");
        crate::gui::jstr(&mut o, st.offer.as_ref().map_or("", |of| of.notes.as_str()));
        o.push_str(",\"manual\":");
        match &st.method {
            Method::Manual(why) if !why.is_empty() => crate::gui::jstr(&mut o, why),
            _ => o.push_str("null"),
        }
        let _ = write!(o, ",\"rev\":{}}}", st.rev);
        o
    }

    /// The page to open for "Release page".
    pub fn page(&self) -> String {
        self.status().offer.map_or(RELEASES_PAGE.to_string(), |o| o.page)
    }
}

// ---------------------------------------------------------------------------
// Restarting into the new version
// ---------------------------------------------------------------------------

/// Set by "Restart now": `main` starts the program again once this one has
/// saved its settings and let go of the audio.
pub static RESTART: AtomicBool = AtomicBool::new(false);

/// The command line for the restarted BananaSplit: the same, but with its
/// window (`--hidden`, and on Windows `--no-open`, start without one).
pub fn restart_args(args: &[String]) -> Vec<String> {
    args.iter().filter(|a| *a != "--hidden" && !(cfg!(windows) && *a == "--no-open")).cloned().collect()
}

/// Start the program again (it waits for this process to end).
/// `drop_env`: variables this process set for itself.
pub fn spawn_restarted(drop_env: &[&str]) -> Result<(), String> {
    let program = self_path().ok_or("cannot find this program's file")?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut c = command(&program);
    c.args(restart_args(&args)).stdout(Stdio::null()).stderr(Stdio::null());
    for v in drop_env.iter().chain(&["DESKTOP_STARTUP_ID", "XDG_ACTIVATION_TOKEN", WAIT_ENV]) {
        c.env_remove(v);
    }
    c.env(WAIT_ENV, std::process::id().to_string());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Out of a job that would end it with this process, where allowed.
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        c.creation_flags(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB);
        if c.spawn().is_ok() {
            return Ok(());
        }
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c.spawn().map(drop).map_err(|e| format!("cannot start {}: {e}", program.display()))
}

/// Another update is still writing it (Linux).
fn pid_alive(pid: &str) -> bool {
    #[cfg(unix)]
    return pid.parse().is_ok_and(alive);
    #[cfg(windows)]
    {
        const SYNCHRONIZE: u32 = 0x0010_0000;
        const WAIT_TIMEOUT: u32 = 0x102;
        let Ok(pid) = pid.parse::<u32>() else { return false };
        // SAFETY: a handle we open and close; a zero wait only asks.
        unsafe {
            let h = OpenProcess(SYNCHRONIZE, 0, pid);
            if h.is_null() {
                return false;
            }
            let running = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
            CloseHandle(h);
            running
        }
    }
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    // a zombie (exited, not yet reaped by its parent) is gone
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
        s.rsplit_once(')').and_then(|(_, r)| r.trim_start().chars().next()).is_some_and(|c| c != 'Z' && c != 'X')
    })
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
    fn WaitForSingleObject(h: *mut std::ffi::c_void, ms: u32) -> u32;
    fn CloseHandle(h: *mut std::ffi::c_void) -> i32;
}

/// A restarted BananaSplit: wait (20 s at most) for the one it replaces to
/// end. Call while the program is still single-threaded.
pub fn wait_for_previous() {
    let Some(v) = std::env::var_os(WAIT_ENV) else { return };
    // SAFETY: called at the start of main, before any thread is started.
    unsafe { std::env::remove_var(WAIT_ENV) };
    let Some(pid) = v.to_str().and_then(|s| s.parse::<u32>().ok()).filter(|&p| p != 0 && p != std::process::id())
    else {
        return;
    };
    #[cfg(unix)]
    {
        let t0 = Instant::now();
        while alive(pid) && t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    #[cfg(windows)]
    {
        const SYNCHRONIZE: u32 = 0x0010_0000;
        // SAFETY: a handle we open and close; WaitForSingleObject only waits.
        unsafe {
            let h = OpenProcess(SYNCHRONIZE, 0, pid);
            if !h.is_null() {
                WaitForSingleObject(h, 20_000);
                CloseHandle(h);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// `bananasplit --update`
// ---------------------------------------------------------------------------

/// Check, and install a new version if there is one (no restart: the next
/// start runs it).
/// `running`: a BananaSplit app runs (Windows: it would start its window
/// from the new file, which is not its version; update it from its menu).
pub fn cli(running: bool) -> Result<(), String> {
    let src = Source::from_env();
    let never = AtomicBool::new(false);
    let method = method();
    println!(
        "bananasplit: this is BananaSplit {VERSION}; checking {} …",
        if src.https { "GitHub" } else { src.api.as_str() }
    );
    let Some(offer) = find(&src, CLI_CACHE, method.package(), &never)?.filter(|o| newer(&o.version, VERSION)) else {
        println!("bananasplit: BananaSplit {VERSION} is the newest version");
        return Ok(());
    };
    println!("bananasplit: BananaSplit {} is available ({})", offer.version, offer.page);
    if let Method::Manual(why) = &method {
        return Err(format!("it cannot install itself here: {why}"));
    }
    if running {
        return Err("BananaSplit is running: update it from its Menu ▸ Check for updates…, or quit it first".into());
    }
    if method == Method::Deb && !src.https {
        return Err("a package is installed only from GitHub, not from a test server".into());
    }
    println!("bananasplit: downloading {} ({:.1} MB) …", offer.asset.name, offer.asset.size as f64 / 1e6);
    let (file, hash) = download(&src, CLI_CACHE, &offer, &never, &mut |_| {})?;
    println!("bananasplit: the release is signed with BananaSplit's key, and the download matches its checksums");
    #[cfg(unix)]
    if method == Method::Deb && !crate::sys::stdin_is_tty() {
        return Err(format!(
            "installing the package needs your password; in a terminal run:  sudo apt install {}",
            file.display()
        ));
    }
    let r = install(&method, &offer, &file, &hash, &never, true);
    if (r.is_ok() || method != Method::Deb)
        && let Some(d) = file.parent()
    {
        clean_cache(d);
    }
    let (r, warnings) = r?;
    println!(
        "bananasplit: installed BananaSplit {} ({r}); a running BananaSplit uses it after a restart",
        offer.version
    );
    for w in warnings {
        println!("bananasplit: note: {w}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_version_is_a_version() {
        assert!(newer(VERSION, "0.0.1") && parse_version(VERSION).is_some());
    }

    #[test]
    fn curl_versions_are_read() {
        assert_eq!(parse_curl_version("curl 8.4.0 (Windows) libcurl/8.4.0 Schannel\nRelease-Date: x\n"), Some((8, 4)));
        assert_eq!(parse_curl_version("curl 7.55.1 (Windows) libcurl/7.55.1 WinSSL\n"), Some((7, 55)));
        assert!(
            parse_curl_version("curl 7.55.1").unwrap() < (7, 70)
                && (7, 70) <= parse_curl_version("curl 7.70.0").unwrap()
        );
        assert_eq!(parse_curl_version("wget 1.21"), None);
        assert_eq!(parse_curl_version(""), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_gui_and_the_command_line_download_to_folders_of_their_own() {
        let (g, c) = (cache_dir(GUI_CACHE).unwrap(), cache_dir(CLI_CACHE).unwrap());
        assert_ne!(g, c);
        assert_eq!(g.parent(), c.parent());
        // cleaning one leaves the other's download alone
        let d = temp("caches");
        let (gd, cd) = (d.join(GUI_CACHE), d.join(CLI_CACHE));
        std::fs::create_dir_all(&gd).unwrap();
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(gd.join("bananasplit_9.9.9_amd64.deb"), b"prepared").unwrap();
        std::fs::write(cd.join("bananasplit-9.9.9-linux-x86_64.tar.gz.part"), b"half").unwrap();
        clean_cache(&cd);
        assert!(gd.join("bananasplit_9.9.9_amd64.deb").is_file());
        assert_eq!(std::fs::read_dir(&cd).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_wget_redirect_to_plain_http_is_seen() {
        let hop = |loc: &str| {
            format!("  HTTP/1.1 302 Found\n  Server: GitHub.com\n  {loc}\n  HTTP/1.1 200 OK\n  Content-Length: 2\n")
        };
        assert!(redirected_to_http(&hop("Location: http://objects.githubusercontent.com/x")));
        assert!(redirected_to_http(&hop("location:HTTP://evil.example/x")));
        assert!(!redirected_to_http(&hop("Location: https://objects.githubusercontent.com/x")));
        assert!(!redirected_to_http(&hop("location: /relative/x")), "same scheme");
        assert!(!redirected_to_http(&hop("location: //host/x")), "same scheme");
        assert!(!redirected_to_http("  HTTP/1.1 200 OK\n  X-Note: Location: http://x\n"), "only the Location header");
        assert!(!redirected_to_http(""));
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bs-update-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn offer_for(file: &Path, digest: Option<[u8; 32]>) -> Offer {
        let size = std::fs::metadata(file).unwrap().len();
        Offer {
            version: "9.9.9".into(),
            tag: "v9.9.9".into(),
            page: RELEASES_PAGE.into(),
            notes: String::new(),
            asset: Asset { name: "bananasplit_9.9.9_amd64.deb".into(), size, url: String::new(), digest },
            sums: None,
            published: None,
            sig: None,
        }
    }

    #[test]
    fn a_download_must_match_every_checksum_the_release_has() {
        let d = temp("verify");
        let f = d.join("bananasplit_9.9.9_amd64.deb");
        std::fs::write(&f, b"the package").unwrap();
        let good = sha256(b"the package");
        let bad = sha256(b"something else");
        let sums = |h: &[u8; 32]| format!("{}  bananasplit_9.9.9_amd64.deb\n", hex(h));
        assert!(verify(&f, &offer_for(&f, Some(good)), None).is_ok());
        assert!(verify(&f, &offer_for(&f, None), Some(&sums(&good))).is_ok());
        assert!(verify(&f, &offer_for(&f, Some(good)), Some(&sums(&good))).is_ok());
        assert!(verify(&f, &offer_for(&f, Some(bad)), None).unwrap_err().contains("does not match GitHub's checksum"));
        assert!(verify(&f, &offer_for(&f, Some(good)), Some(&sums(&bad))).unwrap_err().contains("SHA256SUMS.txt"));
        assert!(verify(&f, &offer_for(&f, None), Some("")).unwrap_err().contains("does not list"));
        assert!(verify(&f, &offer_for(&f, None), None).unwrap_err().contains("no checksum"));
        let mut o = offer_for(&f, Some(good));
        o.asset.size += 1;
        assert!(verify(&f, &o, None).unwrap_err().contains("bytes"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_what_an_update_left_is_cleaned_up() {
        let dead = |_: &str| false;
        let live = |p: &str| p == "77";
        // Windows
        assert!(stale(true, "bananasplit.exe", "bananasplit.exe.old", 5, &dead));
        assert!(stale(true, "bananasplit.exe", "bananasplit.exe.old-123", 5, &dead));
        assert!(stale(true, "bananasplit.exe", "bananasplit.update-123.exe", 5, &dead));
        assert!(!stale(true, "bananasplit.exe", "bananasplit.update-5.exe", 5, &dead), "this process's own");
        assert!(!stale(true, "bananasplit.exe", "bananasplit.update-77.exe", 5, &live), "a running update's");
        assert!(!stale(true, "bananasplit.exe", "bananasplit.exe", 5, &dead));
        assert!(!stale(true, "bananasplit.exe", "bananasplit.exe.old-x", 5, &dead));
        assert!(!stale(true, "bananasplit.exe", "bananasplit.update-.exe", 5, &dead));
        assert!(!stale(true, "bananasplit.exe", "README.md", 5, &dead));
        assert!(!stale(true, "bananasplit.exe", "other.exe.old", 5, &dead));
        assert!(stale(true, "bs-2.8.0.exe", "bs-2.8.0.exe.old", 5, &dead), "named after the program file");
        // Linux
        assert!(stale(false, "bananasplit", ".bananasplit.update-123", 5, &dead));
        assert!(!stale(false, "bananasplit", ".bananasplit.update-5", 5, &dead));
        assert!(!stale(false, "bananasplit", ".bananasplit.update-77", 5, &live));
        assert!(!stale(false, "bananasplit", ".bananasplit.update-12a", 5, &dead));
        assert!(!stale(false, "bananasplit", "bananasplit.exe.old", 5, &dead), "Windows' names are not Linux's");
        assert!(!stale(false, "bananasplit", "bananasplit", 5, &dead));
    }

    #[test]
    fn a_restart_brings_the_window_back() {
        let a: Vec<String> = ["--gui", "--hidden", "--config", "/x/--hidden.ini", "--gui-port", "9000"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(restart_args(&a), ["--gui", "--config", "/x/--hidden.ini", "--gui-port", "9000"]);
        let b: Vec<String> = ["--gui", "--no-open"].iter().map(|s| s.to_string()).collect();
        assert_eq!(restart_args(&b), if cfg!(windows) { vec!["--gui"] } else { vec!["--gui", "--no-open"] });
    }

    /// A one-file HTTP server on 127.0.0.1 for `fetch`: answers every
    /// request with `status` and `body`.
    fn serve(status: &'static str, body: Vec<u8>, hits: usize) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{BufRead, Write};
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..hits {
                let Ok((s, _)) = l.accept() else { break };
                let mut r = std::io::BufReader::new(s);
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                seen.push(head);
                let mut s = r.into_inner();
                let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(&body);
            }
            seen
        });
        (port, h)
    }

    #[test]
    fn downloads_go_through_curl_and_fail_cleanly() {
        if find_tool().is_none() {
            eprintln!("no curl/wget here: skipped");
            return;
        }
        let d = temp("fetch");
        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let (port, h) = serve("200 OK", body.clone(), 1);
        let src = Source::local(&format!("http://127.0.0.1:{port}")).unwrap();
        let never = AtomicBool::new(false);
        let dest = d.join("file");
        let mut last = 0;
        fetch(&src, &format!("http://127.0.0.1:{port}/f"), &dest, body.len() as u64, true, &never, &mut |n| last = n)
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_eq!(last, body.len() as u64, "progress reaches the size");
        let head = h.join().unwrap().join("");
        assert!(head.contains("User-Agent: BananaSplit/"), "{head}");
        assert!(head.contains("Accept: application/vnd.github+json"), "{head}");

        let (port, h) = serve("404 Not Found", b"nope".to_vec(), 1);
        let e = fetch(&src, &format!("http://127.0.0.1:{port}/f"), &dest, 100, false, &never, &mut |_| {}).unwrap_err();
        assert!(e.contains("404") || e.contains("error"), "{e}");
        assert!(!dest.exists() && !d.join("file.part").exists(), "nothing left behind");
        h.join().unwrap();

        let (port, h) = serve("200 OK", body.clone(), 1);
        let e =
            fetch(&src, &format!("http://127.0.0.1:{port}/f"), &dest, 1000, false, &never, &mut |_| {}).unwrap_err();
        assert!(e.contains("larger"), "{e}");
        assert!(!dest.exists());
        h.join().unwrap();

        // nothing listening
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        assert!(fetch(&src, &format!("http://127.0.0.1:{port}/f"), &dest, 1000, false, &never, &mut |_| {}).is_err());
        // https only, when the source is GitHub
        let e =
            fetch(&Source::github(), &format!("http://127.0.0.1:{port}/f"), &dest, 1000, false, &never, &mut |_| {})
                .unwrap_err();
        assert!(!e.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    fn script(path: &Path, text: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_program_is_replaced_only_by_one_that_runs_and_has_the_version() {
        use std::os::unix::fs::PermissionsExt;
        let d = temp("replace");
        let (a, b) = (d.join("app"), d.join("bin"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let (t1, t2) = (a.join("bananasplit"), b.join("bananasplit"));
        std::fs::write(&t1, b"old one").unwrap();
        std::fs::write(&t2, b"old two").unwrap();
        for t in [&t1, &t2] {
            std::fs::set_permissions(t, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let new = d.join("new");
        script(&new, "#!/bin/sh\necho 'BananaSplit 9.9.9'\n");
        // the wrong version: nothing changes, nothing is left behind
        let e = replace(&new, &[t1.clone(), t2.clone()], "9.9.8", &AtomicBool::new(false)).unwrap_err();
        assert!(e.contains("says it is BananaSplit 9.9.9, not 9.9.8"), "{e}");
        assert_eq!(std::fs::read(&t1).unwrap(), b"old one");
        assert_eq!(std::fs::read_dir(&a).unwrap().count(), 1, "the staged file is removed");
        // one that does not run
        let broken = d.join("broken");
        script(&broken, "#!/bin/sh\nexit 3\n");
        assert!(
            replace(&broken, std::slice::from_ref(&t1), "9.9.9", &AtomicBool::new(false))
                .unwrap_err()
                .contains("does not run on this computer")
        );
        assert_eq!(std::fs::read(&t1).unwrap(), b"old one");
        // the right one: both copies, executable
        replace(&new, &[t1.clone(), t2.clone()], "9.9.9", &AtomicBool::new(false)).unwrap();
        for t in [&t1, &t2] {
            assert_eq!(std::fs::read(t).unwrap(), std::fs::read(&new).unwrap());
            assert_eq!(std::fs::metadata(t).unwrap().permissions().mode() & 0o777, 0o755);
        }
        assert_eq!(std::fs::read_dir(&a).unwrap().count(), 1);
        assert!(check_program(&t1, "9.9.9").is_ok());
        assert!(check_program(&t1, "9.9").is_err(), "the whole version");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn a_copy_that_cannot_be_replaced_after_the_running_one_is_a_warning() {
        use std::os::unix::fs::PermissionsExt;
        let d = temp("partial");
        let (a, b) = (d.join("app"), d.join("ro"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let (t1, t2) = (a.join("bananasplit"), b.join("bananasplit"));
        std::fs::write(&t1, b"old").unwrap();
        std::fs::write(&t2, b"old").unwrap();
        let new = d.join("new");
        script(&new, "#!/bin/sh\necho 'BananaSplit 9.9.9'\n");
        // a second "copy" whose folder is gone: it cannot be written
        let gone = d.join("gone").join("bananasplit");
        let w = replace(&new, &[t1.clone(), gone.clone()], "9.9.9", &AtomicBool::new(false)).unwrap();
        assert_eq!(std::fs::read(&t1).unwrap(), std::fs::read(&new).unwrap(), "the running program is new");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("was not updated"), "{w:?}");
        // the first one failing is an error
        assert!(replace(&new, &[gone, t2.clone()], "9.9.9", &AtomicBool::new(false)).is_err());
        assert_eq!(std::fs::read(&t2).unwrap(), b"old");
        // quitting: nothing is renamed any more, and nothing is left behind
        let e = replace(&new, std::slice::from_ref(&t2), "9.9.9", &AtomicBool::new(true)).unwrap_err();
        assert!(e.contains("quitting"), "{e}");
        assert_eq!(std::fs::read(&t2).unwrap(), b"old");
        assert_eq!(std::fs::read_dir(&b).unwrap().count(), 1, "the staged file is removed");
        assert_eq!(std::fs::read(&t2).unwrap(), b"old");
        let _ = std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn a_package_goes_through_pkexec_apt_get() {
        if which("sha256sum").is_none() || which("mktemp").is_none() {
            return;
        }
        let d = temp("deb");
        let log = d.join("log");
        let pkexec = d.join("pkexec");
        let apt = d.join("apt-get");
        let program = d.join("bananasplit");
        let deb = d.join("bananasplit_9.9.9_amd64.deb");
        std::fs::write(&deb, b"deb").unwrap();
        let hash = sha256(b"deb");
        let never = AtomicBool::new(false);
        // pkexec runs the root part as it is; apt-get says what it got
        script(&pkexec, "#!/bin/sh\nexec \"$@\"\n");
        for (code, installs, want) in [(0, true, ""), (100, false, "apt-get failed (exit 100)")] {
            let _ = std::fs::remove_file(&log);
            script(
                &apt,
                &format!(
                    "#!/bin/sh\necho \"$@\" > {log}\ncmp -s \"$4\" {deb} && echo same >> {log}\necho 'E: something' >&2\nexit {code}\n",
                    log = log.display(),
                    deb = deb.display()
                ),
            );
            script(&program, "#!/bin/sh\necho 'BananaSplit 9.9.9'\n");
            let r = install_deb_with(&pkexec, &apt, &program, &deb, &hash, "9.9.9", &never, false);
            let said = std::fs::read_to_string(&log).unwrap();
            let mut lines = said.lines();
            let first = lines.next().unwrap();
            assert!(
                first.starts_with("install -y --no-remove /tmp/bananasplit-update.")
                    && first.ends_with("/bananasplit.deb"),
                "{said}"
            );
            assert_eq!(lines.next(), Some("same"), "a copy of the checked package");
            assert!(!Path::new(first.rsplit(' ').next().unwrap()).exists(), "the copy is removed");
            match installs {
                true => assert!(r.is_ok(), "{r:?}"),
                false => assert!(r.as_ref().unwrap_err().contains(want), "{code}: {r:?}"),
            }
        }
        // the package changed after it was checked: apt-get never runs
        let _ = std::fs::remove_file(&log);
        let r = install_deb_with(&pkexec, &apt, &program, &deb, &sha256(b"other"), "9.9.9", &never, false);
        assert!(r.unwrap_err().contains("changed after it was checked"));
        assert!(!log.exists(), "apt-get did not run");
        // pkexec's own answers
        for (code, want) in [(126, "cancelled"), (127, "could not get permission")] {
            script(&pkexec, &format!("#!/bin/sh\nexit {code}\n"));
            assert!(
                install_deb_with(&pkexec, &apt, &program, &deb, &hash, "9.9.9", &never, false)
                    .unwrap_err()
                    .contains(want),
                "{code}"
            );
        }
        // installed, but the program is not the new version: an error
        script(&pkexec, "#!/bin/sh\nexit 0\n");
        script(&program, "#!/bin/sh\necho 'BananaSplit 2.7.3'\n");
        assert!(
            install_deb_with(&pkexec, &apt, &program, &deb, &hash, "9.9.9", &never, false)
                .unwrap_err()
                .contains("not 9.9.9")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn the_new_program_keeps_the_old_ones_permissions() {
        assert_eq!(program_mode(None), 0o755);
        assert_eq!(program_mode(Some(0o100755)), 0o755);
        assert_eq!(program_mode(Some(0o644)), 0o755, "readable: runnable");
        assert_eq!(program_mode(Some(0o700)), 0o700, "private stays private");
        assert_eq!(program_mode(Some(0o750)), 0o750);
        assert_eq!(program_mode(Some(0o4755)), 0o755, "no set-id bits");
        assert_eq!(program_mode(Some(0o600)), 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn a_package_unpacks_to_its_program_only() {
        if which("tar").is_none() || which("gzip").is_none() {
            return;
        }
        let d = temp("unpack");
        let top = d.join("src").join("bananasplit-9.9.9-linux-x86_64");
        std::fs::create_dir_all(&top).unwrap();
        std::fs::write(top.join("bananasplit"), b"program").unwrap();
        std::fs::write(top.join("install.sh"), b"#!/bin/sh\n").unwrap();
        let tgz = d.join("bananasplit-9.9.9-linux-x86_64.tar.gz");
        let ok = Command::new("tar")
            .arg("-czf")
            .arg(&tgz)
            .arg("-C")
            .arg(d.join("src"))
            .arg("bananasplit-9.9.9-linux-x86_64")
            .status()
            .unwrap();
        assert!(ok.success());
        let bin = unpack(&tgz, "9.9.9").unwrap();
        assert_eq!(std::fs::read(&bin).unwrap(), b"program");
        assert!(!bin.parent().unwrap().join("install.sh").exists(), "only the program");
        assert!(unpack(&tgz, "9.9.8").is_err(), "the version's folder");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn a_zombie_is_not_waited_for() {
        assert!(alive(std::process::id()));
        let mut c = Command::new("sh").arg("-c").arg("exit 0").spawn().unwrap();
        let pid = c.id();
        let t0 = Instant::now();
        while alive(pid) && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!alive(pid), "exited, not yet reaped");
        c.wait().unwrap();
        assert!(!alive(pid));
    }

    #[test]
    fn the_updater_is_idle_until_asked() {
        let u = Updater::start();
        // no automatic check while it is off, however long it runs
        for _ in 0..3 {
            u.tick(false);
        }
        assert_eq!(u.status().phase, Phase::Idle);
        assert!(u.install().unwrap_err().contains("no new version"));
        let mut o = String::new();
        u.json(&mut o, false);
        assert!(
            o.starts_with(&format!("{{\"auto\":false,\"phase\":\"idle\",\"current\":\"{VERSION}\",\"latest\":null,")),
            "{o}"
        );
        Json::parse(&o).unwrap();
        Json::parse(&u.details_json()).unwrap();
        u.shutdown();
        assert!(u.check().is_err(), "stopped");
    }
}
