//! The Windows side of [`apphome`]: which apps have sound open, where
//! Windows puts each one, and putting one on a strip's cable. An app's own
//! playback device is what Windows' Volume mixer sets: the per-process
//! "persisted default endpoint" of the undocumented
//! `IAudioPolicyConfigFactory` (see `ffi.rs`). Without it (an older Windows,
//! or a build that changed it), app restore is off and says so once.

use super::apphome::{self, App, AppHomes};
use super::ffi::*;
use super::{chanmenu, route, wasapi};
use bananasplit::config::Config;
use std::ffi::c_void;
use std::path::Path;
use std::time::{Duration, Instant};

const MMDEVAPI_TOKEN: &str = r"\\?\SWD#MMDEVAPI#";
const DEVINTERFACE_AUDIO_RENDER: &str = "#{e6327cad-dcec-4949-ae8a-991e976a79d2}";

/// How often to look at the apps.
const EVERY: Duration = Duration::from_secs(2);

/// The device path the policy wants for a playback endpoint ID.
pub fn policy_id(endpoint: &str) -> String {
    format!("{MMDEVAPI_TOKEN}{endpoint}{DEVINTERFACE_AUDIO_RENDER}")
}

/// The endpoint ID in a device path the policy gives ("" stays "").
pub fn endpoint_id(policy: &str) -> &str {
    let s = policy.strip_prefix(MMDEVAPI_TOKEN).unwrap_or(policy);
    s.strip_suffix(DEVINTERFACE_AUDIO_RENDER).unwrap_or(s)
}

/// An HSTRING made here, deleted on drop.
struct HString(HSTRING);

impl HString {
    fn new(s: &str) -> Result<HString, String> {
        let w: Vec<u16> = s.encode_utf16().collect();
        let mut h: HSTRING = std::ptr::null_mut();
        // SAFETY: `w` holds `w.len()` UTF-16 units and outlives the call (the string is copied); `h` is a local out-pointer.
        let hr = unsafe { WindowsCreateString(w.as_ptr(), w.len() as u32, &mut h) };
        if hr < 0 { Err(format!("WindowsCreateString: {}", hr_text(hr))) } else { Ok(HString(h)) }
    }
}

impl Drop for HString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` is an HSTRING this value owns (made by WindowsCreateString or handed out to us), deleted once.
            unsafe { WindowsDeleteString(self.0) };
        }
    }
}

/// The text of an HSTRING (NULL is "").
fn hstring_text(h: HSTRING) -> String {
    if h.is_null() {
        return String::new();
    }
    let mut n = 0u32;
    // SAFETY: `h` is a live HSTRING; `n` is a local out-pointer.
    let p = unsafe { WindowsGetStringRawBuffer(h, &mut n) };
    if p.is_null() {
        return String::new();
    }
    // SAFETY: the raw buffer holds `n` UTF-16 units while `h` lives (it does, for this call).
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n as usize) })
}

/// `IAudioPolicyConfigFactory`.
struct Policy(Com<IAudioPolicyConfigFactoryVtbl>);

impl Policy {
    /// The factory, on a thread in the multithreaded apartment.
    fn new() -> Result<Policy, String> {
        let class = HString::new(AUDIO_POLICY_CONFIG_CLASS)?;
        let mut last = 0;
        for iid in [&IID_IAudioPolicyConfigFactory, &IID_IAudioPolicyConfigFactoryDownlevel] {
            let mut p: *mut c_void = std::ptr::null_mut();
            // SAFETY: COM is up on this thread (caller); `class` is a live HSTRING, the IID a static, `p` a local out-pointer.
            let hr = unsafe { RoGetActivationFactory(class.0, iid, &mut p) };
            if hr >= 0 {
                // SAFETY: `p` is the owned factory RoGetActivationFactory returned for this IID, whose layout
                // both IIDs share (`IAudioPolicyConfigFactoryVtbl`), on an MTA thread.
                if let Some(c) = unsafe { Com::from_raw(p) } {
                    return Ok(Policy(c));
                }
            }
            last = hr;
        }
        Err(format!("this Windows has no per-app sound devices ({})", hr_text(last)))
    }

    /// The playback endpoint ID Windows gives process `pid`, "" when it
    /// follows the default.
    fn get(&self, pid: u32) -> Result<String, String> {
        let mut h: HSTRING = std::ptr::null_mut();
        // SAFETY: `self.0` owns a live factory with this vtable; `h` is a local out-pointer whose HSTRING we then own.
        let hr = unsafe {
            (self.0.vt().GetPersistedDefaultAudioEndpoint)(self.0.as_ptr(), pid, eRender, eMultimedia, &mut h)
        };
        let h = HString(h); // deleted on drop, also after a failure that set it
        if hr < 0 {
            return Err(hr_text(hr));
        }
        Ok(endpoint_id(&hstring_text(h.0)).to_string())
    }

    /// Give process `pid` the playback endpoint `endpoint`, the way the
    /// Volume mixer does (for both roles it sets).
    fn set(&self, pid: u32, endpoint: &str) -> Result<(), String> {
        let path = HString::new(&policy_id(endpoint))?;
        for role in [eMultimedia, eConsole] {
            // SAFETY: `self.0` owns a live factory with this vtable; `path` is a live HSTRING the call only reads.
            let hr =
                unsafe { (self.0.vt().SetPersistedDefaultAudioEndpoint)(self.0.as_ptr(), pid, eRender, role, path.0) };
            if hr < 0 {
                return Err(hr_text(hr));
            }
        }
        Ok(())
    }
}

/// The program file of process `pid` (empty if it can't be opened).
fn program(pid: u32) -> String {
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    // SAFETY: plain call; the handle, if any, is closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return String::new();
    }
    let mut buf = [0u16; 1024];
    let mut n = buf.len() as u32;
    // SAFETY: `h` is a live process handle; `buf` holds `n` u16s and `n` is updated to the length written.
    let ok = unsafe { QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut n) };
    // SAFETY: `h` came from OpenProcess above and is closed once.
    unsafe { CloseHandle(h) };
    if ok == 0 { String::new() } else { String::from_utf16_lossy(&buf[..(n as usize).min(buf.len())]) }
}

/// Puts apps back on their strips, from the UI loops (see [`apphome`]).
pub struct AppRestore {
    homes: AppHomes,
    policy: Option<Policy>,
    /// Why there is no policy, until said once.
    why: Option<String>,
    last: Instant,
    // Declared last, so the factory is released before COM is.
    _com: Option<ComInit>,
}

impl AppRestore {
    /// For the config file `config`; call [`AppRestore::tick`] on the
    /// thread this was made on.
    pub fn new(config: &Path) -> AppRestore {
        let com = ComInit::mta();
        let (policy, why) = match com.as_ref().map_err(String::clone).and_then(|_| Policy::new()) {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(format!("apps are not put back on their strips: {e}"))),
        };
        AppRestore { homes: AppHomes::load(config), policy, why, last: Instant::now() - EVERY, _com: com.ok() }
    }

    /// Every couple of seconds: look at the apps with sound open and put
    /// noted ones back on their strips. Lines for the user.
    pub fn tick(&mut self, cfg: &Config, names: &[String]) -> Vec<String> {
        if let Some(w) = self.why.take() {
            return vec![w];
        }
        if self.last.elapsed() < EVERY {
            return Vec::new();
        }
        self.last = Instant::now();
        let Some(policy) = &self.policy else { return Vec::new() };
        let Ok(_com) = ComInit::mta() else { return Vec::new() };
        let Ok(en) = wasapi::Enumerator::new() else { return Vec::new() };
        let (Ok(caps), Ok(rens)) = (en.devices(true), en.devices(false)) else { return Vec::new() };
        let me = std::process::id();
        let mut pids: Vec<u32> = Vec::new();
        for d in &rens {
            for pid in d.session_pids() {
                if pid != me && !pids.contains(&pid) {
                    pids.push(pid);
                }
            }
        }
        let apps: Vec<App> = pids
            .into_iter()
            .filter_map(|pid| {
                let key = apphome::key(&program(pid));
                let set_to = policy.get(pid).ok()?;
                (!key.is_empty()).then_some(App { pid, key, set_to })
            })
            .collect();
        let cap_info: Vec<route::DevInfo> = caps.into_iter().map(|d| d.info).collect();
        let ren_info: Vec<route::DevInfo> = rens.into_iter().map(|d| d.info).collect();
        let cables: Vec<Option<String>> = chanmenu::strip_inputs(cfg, &cap_info, &ren_info)
            .into_iter()
            .map(|name| name.and_then(|n| ren_info.iter().find(|d| d.name == n)).map(|d| d.id.clone()))
            .collect();
        // (endpoint IDs hold GUIDs: Windows may print them in either case)
        let present = |id: &str| ren_info.iter().any(|d| d.id.eq_ignore_ascii_case(id));
        let cable = |id: &str| ren_info.iter().any(|d| d.id.eq_ignore_ascii_case(id) && route::is_virtual(&d.name));
        self.homes.poll(&apps, &cables, present, cable, names, |pid, id| policy.set(pid, id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_paths_round_trip() {
        let id = "{0.0.0.00000000}.{8a1f8a8e-0000-4b1c-9d0e-123456789abc}";
        let p = policy_id(id);
        assert_eq!(
            p,
            r"\\?\SWD#MMDEVAPI#{0.0.0.00000000}.{8a1f8a8e-0000-4b1c-9d0e-123456789abc}#{e6327cad-dcec-4949-ae8a-991e976a79d2}"
        );
        assert_eq!(endpoint_id(&p), id);
        assert_eq!(endpoint_id(""), "");
    }
}
