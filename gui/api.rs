//! What the API says when it cannot do what was asked: a status and a message
//! ([`ApiError`]), and the one way it is written on the wire.
//!
//! Every refusal of the server (a bad request, a command that failed, a missing
//! page, a request from another site) is an `ApiError`. Its body is the message
//! as plain text, or, for a client that says `Accept: application/json`,
//! `{"error":"<message>"}`. (Only once the request has been read: a refusal of
//! the connection or of a request that cannot be read is always plain text.) A
//! request that works answers as it always did: plain text for a command, JSON for
//! a state or a list.

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

    /// The body of the answer for a client that asked for JSON: `{"error":"..."}`.
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

/// Does a request's `Accept` header ask for JSON? Only by naming `application/json`:
/// a client that accepts anything (`*/*`, what a browser's `fetch` and `curl` send)
/// gets the plain text.
pub fn accepts_json(accept: Option<&str>) -> bool {
    accept.is_some_and(|a| {
        a.split(',').any(|t| t.split(';').next().is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json")))
    })
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
    fn only_a_client_that_names_json_gets_json() {
        assert!(accepts_json(Some("application/json")));
        assert!(accepts_json(Some("Application/JSON")));
        assert!(accepts_json(Some("text/html, application/json;q=0.9, */*;q=0.1")));
        assert!(accepts_json(Some("application/json; charset=utf-8")));
        assert!(!accepts_json(None));
        assert!(!accepts_json(Some("*/*")), "what curl and a browser's fetch send");
        assert!(!accepts_json(Some("text/plain")));
        assert!(!accepts_json(Some("application/jsonp")));
        assert!(!accepts_json(Some("")));
    }
}
