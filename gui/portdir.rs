//! Where a running GUI notes its port (`<folder>/bananasplit/<client>.port`),
//! so that launching it again and `--quit` find it when 8790 was busy. Which
//! folder, per system, is decided here from the environment values the app
//! passes in; reading the environment is the app's `gui_platform::port_dir`.
//!
//! Each choice is compiled for the system that uses it, and under `cargo test`
//! on every system.

use std::path::{Path, PathBuf};

/// The note for the GUI of JACK client `client` (any other characters than
/// letters, digits, `-` and `_` become `_`) under `dir`.
pub fn file(dir: &Path, client: &str) -> PathBuf {
    let name: String =
        client.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    dir.join("bananasplit").join(format!("{name}.port"))
}

/// Linux: `$XDG_RUNTIME_DIR` if it is an absolute path (in a Flatpak only
/// `$XDG_RUNTIME_DIR/app/<app ID>` is shared by its instances), else
/// `~/.cache`.
#[cfg(any(target_os = "linux", all(unix, test)))]
pub fn linux(
    xdg_runtime_dir: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    flatpak_id: Option<String>,
) -> Option<PathBuf> {
    xdg_runtime_dir
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| match flatpak_id {
            Some(id) => p.join("app").join(id),
            None => p,
        })
        .or_else(|| home.filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".cache")))
}

/// Windows: `%LOCALAPPDATA%`, else `%APPDATA%`, else `%USERPROFILE%\AppData\Local`,
/// else the temp folder.
#[cfg(any(windows, test))]
pub fn windows(
    local_app_data: Option<std::ffi::OsString>,
    app_data: Option<std::ffi::OsString>,
    user_profile: Option<std::ffi::OsString>,
    temp: PathBuf,
) -> PathBuf {
    local_app_data
        .map(PathBuf::from)
        .or_else(|| app_data.map(PathBuf::from))
        .or_else(|| user_profile.map(|h| PathBuf::from(h).join("AppData").join("Local")))
        .unwrap_or(temp)
}

/// macOS: `~/Library/Caches`.
#[cfg(any(target_os = "macos", test))]
pub fn macos(home: Option<std::ffi::OsString>) -> Option<PathBuf> {
    home.filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join("Library").join("Caches"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn the_note_is_named_after_the_client() {
        let f = file(Path::new("/run/user/1000"), "Banana Split/2");
        assert_eq!(f, Path::new("/run/user/1000").join("bananasplit").join("Banana_Split_2.port"));
        assert_eq!(file(Path::new("/d"), "a-b_C9").file_name().unwrap(), "a-b_C9.port");
        assert_eq!(file(Path::new("/d"), "é").file_name().unwrap(), "_.port");
    }

    /// (On a Windows host `/run/user/1000` is not an absolute path, which is what
    /// the function asks: Unix paths are only meaningful on a Unix host.)
    #[cfg(unix)]
    #[test]
    fn linux_prefers_the_runtime_dir() {
        assert_eq!(linux(os("/run/user/1000"), os("/home/u"), None), Some(PathBuf::from("/run/user/1000")));
        // in a Flatpak only the app's own folder is shared by its instances
        let id = Some("io.github.LudaLunizz.BananaSplit".to_string());
        assert_eq!(
            linux(os("/run/user/1000"), os("/home/u"), id),
            Some(PathBuf::from("/run/user/1000/app/io.github.LudaLunizz.BananaSplit"))
        );
        // a relative runtime dir is no runtime dir; so is a missing one
        assert_eq!(linux(os("run/user"), os("/home/u"), None), Some(PathBuf::from("/home/u/.cache")));
        assert_eq!(linux(None, os("/home/u"), None), Some(PathBuf::from("/home/u/.cache")));
        // (a Flatpak id does not turn the home folder into a shared one)
        assert_eq!(linux(None, os("/home/u"), Some("x.y".into())), Some(PathBuf::from("/home/u/.cache")));
        // nowhere to put it
        assert_eq!(linux(None, os(""), None), None);
        assert_eq!(linux(None, None, None), None);
    }

    #[test]
    fn windows_goes_down_the_list() {
        let temp = PathBuf::from(r"C:\Temp");
        assert_eq!(windows(os("L"), os("A"), os("P"), temp.clone()), PathBuf::from("L"));
        assert_eq!(windows(None, os("A"), os("P"), temp.clone()), PathBuf::from("A"));
        assert_eq!(windows(None, None, os("P"), temp.clone()), PathBuf::from("P").join("AppData").join("Local"));
        assert_eq!(windows(None, None, None, temp.clone()), temp);
    }

    #[test]
    fn macos_keeps_it_in_the_caches() {
        assert_eq!(macos(os("/Users/u")), Some(PathBuf::from("/Users/u/Library/Caches")));
        assert_eq!(macos(os("")), None);
        assert_eq!(macos(None), None);
    }
}
