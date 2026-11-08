//! What the API says when it cannot do what was asked: a status and a message
//! ([`ApiError`]), and the one way it is written on the wire.
//!
//! Every refusal of the server (a bad request, a command that failed, a missing
//! page, a request from another site) is an `ApiError`, and its body is
//! `{"error":"<message>"}` (`application/json`). A request that works answers as
//! it always did: plain text for a command, JSON for a state or a list.

use std::fmt;

/// A refusal: the HTTP status and what to tell the person.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub status: u16,
    pub msg: String,
}

impl ApiError {
    pub fn new(status: u16, msg: impl Into<String>) -> ApiError {
        ApiError { status, msg: msg.into() }
    }

    /// The body of the answer: `{"error":"..."}`.
    pub fn json(&self) -> String {
        let mut o = String::from("{\"error\":");
        super::jstr(&mut o, &self.msg);
        o.push('}');
        o
    }
}

/// A command that does not work is a bad request, whichever step found out.
impl From<String> for ApiError {
    fn from(msg: String) -> ApiError {
        ApiError::new(400, msg)
    }
}

impl From<&str> for ApiError {
    fn from(msg: &str) -> ApiError {
        ApiError::new(400, msg)
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

/// The message in the body of an answer that refused: what `ApiError::json`
/// wrote, or, for an answer that is not that (an older BananaSplit, another
/// program on the port), the text as it is. `bananasplit --quit` prints it.
pub fn message_of(body: &str) -> String {
    json_error(body.trim()).unwrap_or_else(|| body.to_string())
}

/// `{"error":"..."}` and nothing else, decoded; `None` for anything different.
fn json_error(body: &str) -> Option<String> {
    let rest = body.strip_prefix("{\"error\":\"")?.strip_suffix("\"}")?;
    let mut out = String::with_capacity(rest.len());
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return None, // an unescaped quote ends the string before its end
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok().filter(|_| hex.len() == 4)?)?);
                }
                _ => return None,
            },
            c => out.push(c),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_is_one_json_object() {
        let e = ApiError::new(403, "wrong Host");
        assert_eq!((e.status, e.json().as_str()), (403, r#"{"error":"wrong Host"}"#));
        // quotes, backslashes, newlines and what a page must not read as markup are escaped
        let e = ApiError::new(400, "unknown op \"zz\"\nline <two> \\ & done");
        assert_eq!(e.json(), r#"{"error":"unknown op \"zz\"\nline \u003ctwo\u003e \\ \u0026 done"}"#);
        // a string or a str is a bad request
        assert_eq!(ApiError::from("bad bus"), ApiError::new(400, "bad bus"));
        assert_eq!(ApiError::from(String::from("x")).status, 400);
        assert_eq!(ApiError::new(503, "no devices").to_string(), "no devices");
    }

    #[test]
    fn what_the_server_writes_is_read_back() {
        for msg in ["wrong Host", "", "unknown op \"cables\"", "a\\b", "tab\there\r\nand <b>é</b> & ü", "\u{1}\u{2028}"]
        {
            let body = ApiError::new(400, msg).json();
            assert_eq!(message_of(&body), msg, "{body}");
        }
    }

    #[test]
    fn other_bodies_are_the_text_they_are() {
        // an older BananaSplit answered with the bare message
        assert_eq!(message_of("forbidden"), "forbidden");
        assert_eq!(message_of("unknown op \"zzz\""), "unknown op \"zzz\"");
        assert_eq!(message_of(""), "");
        // not exactly one error object: left alone
        for body in [
            r#"{"error":"a","more":1}"#,
            r#"{"error":"a"#,
            r#"{"error":"a"b"}"#,
            r#"{"error":"\x"}"#,
            r#"{"error":"\u00zz"}"#,
        ] {
            assert_eq!(message_of(body), body);
        }
        // a trailing newline after the object does not matter
        assert_eq!(message_of("{\"error\":\"x\"}\n"), "x");
    }
}
