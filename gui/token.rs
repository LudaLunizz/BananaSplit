//! The key for the operations with effects outside the mixer (#166).
//!
//! Every start makes a new random token (32 bytes from the system's random
//! source, as 64 hex digits), writes it to `bananasplit/<client>.token` next
//! to the port note, readable by this user only, and removes it on quit. A
//! request for a protected operation (quit, install an update, start a
//! recording, open a folder, send audio to another computer, ...; the list is
//! [`Cmd::needs_token`](super::cmd::Cmd::needs_token) and
//! [`Target::needs_token`](super::v1::Target::needs_token)) must carry it in
//! the `X-BananaSplit-Token` header. Mixer controls do not need it.
//!
//! The page, the deck and the window get it in the URL's fragment (`#t=...`,
//! which the browser never sends to a server), read it once, take it out of
//! the address bar and keep it in `sessionStorage`. A window process gets it
//! in its environment (`ENV`), not on its command line.
//!
//! This only keeps out programs that can reach 127.0.0.1 but cannot read the
//! user's files (a sandboxed app); a program that runs as the user unconfined
//! can read the file. Other users are refused before this (`peer.rs`).

use super::api::ApiError;
use std::path::Path;

/// The request header that carries the token.
pub const HEADER: &str = "X-BananaSplit-Token";
/// The environment variable a window process gets the token in.
#[cfg_attr(target_os = "macos", allow(dead_code, reason = "macOS has no window process"))]
pub const ENV: &str = "BANANASPLIT_PAGE_TOKEN";
/// Random bytes in a token (256 bits).
pub const BYTES: usize = 32;

/// A token: `bytes` as lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    bytes.iter().flat_map(|b| [D[(b >> 4) as usize] as char, D[(b & 15) as usize] as char]).collect()
}

/// Does `t` look like a token (64 lowercase hex digits)? A file or a variable
/// that holds anything else is not used.
pub fn valid(t: &str) -> bool {
    t.len() == 2 * BYTES && t.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `a == b` in a time that does not depend on where they differ. An empty
/// `expected` matches nothing (no token: the protected operations are closed).
pub fn same(given: &str, expected: &str) -> bool {
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    if b.is_empty() || a.len() != b.len() {
        return false; // the length is no secret: every token is 64 digits
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

/// `url` with the token in its fragment (`#t=...`).
pub fn with_fragment(url: &str, token: &str) -> String {
    if token.is_empty() { url.to_string() } else { format!("{url}#t={token}") }
}

/// `url` without a `#t=...` fragment, and the token that was in it.
#[cfg_attr(target_os = "macos", allow(dead_code, reason = "macOS has no window process"))]
pub fn split_fragment(url: &str) -> (&str, Option<&str>) {
    match url.split_once("#t=") {
        Some((base, t)) if valid(t) => (base, Some(t)),
        _ => (url, None),
    }
}

/// In a window process: its URL with the token from the environment (`ENV`),
/// if one was passed and the URL has no fragment yet.
#[cfg_attr(target_os = "macos", allow(dead_code, reason = "macOS has no window process"))]
pub fn url_from_env(url: &str, env: Option<std::ffi::OsString>) -> String {
    match env.as_ref().and_then(|e| e.to_str()).filter(|t| valid(t)) {
        Some(t) if !url.contains('#') => with_fragment(url, t),
        _ => url.to_string(),
    }
}

/// The token in a token file's text (it ends with a newline), if it is one.
pub fn parse_file(text: &str) -> Option<&str> {
    Some(text.trim()).filter(|t| valid(t))
}

/// The refusal of a protected operation without the right token.
pub fn refused(file: Option<&Path>) -> ApiError {
    let where_ = match file {
        Some(f) => format!("the file {}", f.display()),
        None => "the file bananasplit/<client>.token in BananaSplit's runtime folder".to_string(),
    };
    ApiError::new(
        403,
        format!(
            "this needs BananaSplit's token: open BananaSplit's window to use it, or send the {HEADER} \
             header with the contents of {where_} (it changes at every start)"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    const T: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    #[test]
    fn a_token_is_64_hex_digits() {
        assert_eq!(hex(&[0, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert!(valid(&hex(&[7; BYTES])));
        assert!(valid(T));
        assert!(!valid(&T[1..]), "too short");
        assert!(!valid(&format!("{T}0")), "too long");
        assert!(!valid(&T.to_uppercase()), "lowercase only");
        assert!(!valid(&T.replace('0', "g")));
        assert!(!valid(""));
    }

    #[test]
    fn only_the_same_token_matches() {
        assert!(same(T, T));
        let mut other = T.to_string();
        other.replace_range(63.., "0");
        assert!(!same(&other, T), "the last digit differs");
        other = T.to_string();
        other.replace_range(..1, "1");
        assert!(!same(&other, T), "the first digit differs");
        assert!(!same("", T), "missing");
        assert!(!same(&T[..63], T), "shorter");
        assert!(!same(&format!("{T}0"), T), "longer");
        assert!(!same("", ""), "no token: nothing matches");
        assert!(!same("x", ""));
    }

    #[test]
    fn the_fragment_carries_it() {
        let u = with_fragment("http://127.0.0.1:8790/", T);
        assert_eq!(u, format!("http://127.0.0.1:8790/#t={T}"));
        assert_eq!(split_fragment(&u), ("http://127.0.0.1:8790/", Some(T)));
        assert_eq!(with_fragment("http://127.0.0.1:8790/", ""), "http://127.0.0.1:8790/");
        assert_eq!(split_fragment("http://127.0.0.1:8790/"), ("http://127.0.0.1:8790/", None));
        // not a token: left as it is
        assert_eq!(split_fragment("http://127.0.0.1:8790/#t=zz"), ("http://127.0.0.1:8790/#t=zz", None));
        assert_eq!(split_fragment("http://127.0.0.1:8790/deck#buses"), ("http://127.0.0.1:8790/deck#buses", None));
        // the query string never carries it
        assert!(!u.contains('?'));
    }

    #[test]
    fn a_window_process_takes_it_from_its_environment() {
        let url = "http://127.0.0.1:8790/";
        assert_eq!(url_from_env(url, Some(OsString::from(T))), format!("{url}#t={T}"));
        assert_eq!(url_from_env(url, None), url);
        assert_eq!(url_from_env(url, Some(OsString::from("not a token"))), url);
        // a URL that has a fragment keeps it
        assert_eq!(url_from_env("http://127.0.0.1:8790/#t=x", Some(OsString::from(T))), "http://127.0.0.1:8790/#t=x");
    }

    #[test]
    fn a_file_holds_one_token() {
        assert_eq!(parse_file(&format!("{T}\n")), Some(T));
        assert_eq!(parse_file(T), Some(T));
        assert_eq!(parse_file("8790\n"), None);
        assert_eq!(parse_file(""), None);
    }

    #[test]
    fn the_refusal_says_where_the_token_is() {
        let e = refused(Some(Path::new("/run/user/1000/bananasplit/bananasplit.token")));
        assert_eq!(e.status, 403);
        assert!(e.msg.contains("/run/user/1000/bananasplit/bananasplit.token"), "{}", e.msg);
        assert!(e.msg.contains(HEADER) && e.msg.contains("open BananaSplit's window"), "{}", e.msg);
        assert!(refused(None).msg.contains("bananasplit/<client>.token"));
    }
}
