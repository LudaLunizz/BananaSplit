//! The error type the file-touching parts of the core share.
//!
//! The core has no dependencies, so these implement `Display` and
//! `std::error::Error` by hand instead of through `thiserror`. Every error
//! here prints exactly the text the `String` it replaced did, and converts
//! to a `String` (`From`), so callers that show the message or use `?` in a
//! function returning `Result<_, String>` keep working.

use std::fmt;
use std::path::{Path, PathBuf};

/// An I/O failure and the file or folder it happened on.
#[derive(Debug)]
#[non_exhaustive]
pub struct IoError {
    path: PathBuf,
    source: std::io::Error,
}

impl IoError {
    /// `source` happened while working on `path`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, source: std::io::Error) -> IoError {
        IoError { path: path.into(), source }
    }

    /// The file or folder the operation was on.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What the operating system reported, for example `NotFound`.
    #[must_use]
    pub fn kind(&self) -> std::io::ErrorKind {
        self.source.kind()
    }
}

impl fmt::Display for IoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.source)
    }
}

impl std::error::Error for IoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Implements `From<$ty> for String` for an error type that shows itself.
macro_rules! into_string {
    ($($ty:ty),+ $(,)?) => {$(
        impl From<$ty> for String {
            fn from(e: $ty) -> String {
                e.to_string()
            }
        }
    )+};
}
pub(crate) use into_string;

into_string!(IoError);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_the_path_then_the_reason() {
        let e = IoError::new("/nope/x.ini", std::io::Error::from(std::io::ErrorKind::NotFound));
        assert!(e.to_string().starts_with("/nope/x.ini: "), "{e}");
        assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(e.path(), Path::new("/nope/x.ini"));
        assert!(std::error::Error::source(&e).is_some());
        let s: String = e.into();
        assert!(s.starts_with("/nope/x.ini: "));
    }
}
