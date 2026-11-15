//! A small JSON reader for the bodies of `POST /api/v1/command` and the MIDI
//! bindings file: objects, arrays, strings, numbers, `true`, `false`, `null`.
//! Writing JSON stays with `jstr`/`jnum` in `mod.rs`.
//!
//! Limits keep a hostile body cheap: nesting is at most [`MAX_DEPTH`] deep, and
//! the body itself is already limited by the server (64 KB).

use std::fmt::Write as _;

/// How deep objects and arrays may nest.
pub const MAX_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    /// The fields in the order they were written (a repeated name keeps the first).
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// The field `k` of an object.
    pub fn get(&self, k: &str) -> Option<&Value> {
        match self {
            Value::Obj(f) => f.iter().find(|(n, _)| n == k).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        if let Value::Str(s) = self { Some(s) } else { None }
    }
    pub fn as_bool(&self) -> Option<bool> {
        if let Value::Bool(b) = self { Some(*b) } else { None }
    }
    /// What a person would call this value's type, for error messages.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "true/false",
            Value::Num(_) => "a number",
            Value::Str(_) => "text",
            Value::Arr(_) => "a list",
            Value::Obj(_) => "an object",
        }
    }
}

/// Read one JSON value (with nothing but white space after it).
pub fn parse(s: &str) -> Result<Value, String> {
    let mut p = Parser { b: s.as_bytes(), i: 0 };
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(p.err("text after the JSON value"));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn err(&self, what: &str) -> String {
        format!("bad JSON at byte {}: {what}", self.i)
    }
    fn ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.b.get(self.i) {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn word(&mut self, w: &str, v: Value) -> Result<Value, String> {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(v)
        } else {
            Err(self.err("unknown word"))
        }
    }
    fn value(&mut self, depth: usize) -> Result<Value, String> {
        self.ws();
        match self.b.get(self.i) {
            None => Err(self.err("it ends too early")),
            Some(b'n') => self.word("null", Value::Null),
            Some(b't') => self.word("true", Value::Bool(true)),
            Some(b'f') => self.word("false", Value::Bool(false)),
            Some(b'"') => self.string().map(Value::Str),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(c @ (b'[' | b'{')) => {
                if depth >= MAX_DEPTH {
                    return Err(self.err("nested too deep"));
                }
                let obj = *c == b'{';
                self.i += 1;
                let close = if obj { b'}' } else { b']' };
                let (mut arr, mut fields) = (Vec::new(), Vec::new());
                if self.eat(close) {
                    return Ok(if obj { Value::Obj(fields) } else { Value::Arr(arr) });
                }
                loop {
                    if obj {
                        self.ws();
                        if self.b.get(self.i) != Some(&b'"') {
                            return Err(self.err("expected a field name"));
                        }
                        let k = self.string()?;
                        if !self.eat(b':') {
                            return Err(self.err("expected ':'"));
                        }
                        let v = self.value(depth + 1)?;
                        if !fields.iter().any(|(n, _): &(String, Value)| *n == k) {
                            fields.push((k, v));
                        }
                    } else {
                        arr.push(self.value(depth + 1)?);
                    }
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(close) {
                        return Ok(if obj { Value::Obj(fields) } else { Value::Arr(arr) });
                    }
                    return Err(self.err(if obj { "expected ',' or '}'" } else { "expected ',' or ']'" }));
                }
            }
            Some(_) => Err(self.err("unexpected character")),
        }
    }
    fn number(&mut self) -> Result<Value, String> {
        let start = self.i;
        while let Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') = self.b.get(self.i) {
            self.i += 1;
        }
        let t = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| self.err("bad number"))?;
        // Rust's parser takes a few forms JSON does not ("1.", ".5" cannot
        // start here, "+1" cannot either); those are harmless to accept.
        match t.parse::<f64>() {
            Ok(n) if n.is_finite() => Ok(Value::Num(n)),
            _ => Err(self.err("bad number")),
        }
    }
    fn hex4(&mut self) -> Result<u32, String> {
        let h = self.b.get(self.i..self.i + 4).ok_or_else(|| self.err("short \\u escape"))?;
        let h = std::str::from_utf8(h).map_err(|_| self.err("bad \\u escape"))?;
        let v = u32::from_str_radix(h, 16).map_err(|_| self.err("bad \\u escape"))?;
        self.i += 4;
        Ok(v)
    }
    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // the opening quote
        let mut o = String::new();
        loop {
            let start = self.i;
            while let Some(&c) = self.b.get(self.i) {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                self.i += 1;
            }
            o.push_str(std::str::from_utf8(&self.b[start..self.i]).map_err(|_| self.err("not UTF-8"))?);
            match self.b.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(o);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let e = *self.b.get(self.i).ok_or_else(|| self.err("it ends too early"))?;
                    self.i += 1;
                    match e {
                        b'"' => o.push('"'),
                        b'\\' => o.push('\\'),
                        b'/' => o.push('/'),
                        b'b' => o.push('\u{8}'),
                        b'f' => o.push('\u{c}'),
                        b'n' => o.push('\n'),
                        b'r' => o.push('\r'),
                        b't' => o.push('\t'),
                        b'u' => {
                            let mut c = self.hex4()?;
                            if (0xD800..0xDC00).contains(&c) && self.b[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(self.err("bad surrogate pair"));
                                }
                                c = 0x10000 + ((c - 0xD800) << 10) + (lo - 0xDC00);
                            }
                            o.push(char::from_u32(c).ok_or_else(|| self.err("bad \\u escape"))?);
                        }
                        _ => return Err(self.err("bad escape")),
                    }
                }
                Some(_) => return Err(self.err("control character in text")),
                None => return Err(self.err("text without its closing quote")),
            }
        }
    }
}

/// Write `v` as JSON (used for the bindings file).
pub fn write(o: &mut String, v: &Value) {
    match v {
        Value::Null => o.push_str("null"),
        Value::Bool(b) => o.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) => {
            let n = if n.is_finite() { *n } else { 0.0 };
            let _ = write!(o, "{n}");
        }
        Value::Str(s) => super::jstr(o, s),
        Value::Arr(a) => {
            o.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }
                write(o, x);
            }
            o.push(']');
        }
        Value::Obj(f) => {
            o.push('{');
            for (i, (k, x)) in f.iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }
                super::jstr(o, k);
                o.push(':');
                write(o, x);
            }
            o.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_clients_send() {
        let v = parse(
            r#" [{"cmd":"set","path":"/strips/1/gain_db","value":-6.5},{"cmd":"toggle","x":[true,false,null]}] "#,
        )
        .unwrap();
        let Value::Arr(a) = &v else { panic!() };
        assert_eq!(a[0].get("value"), Some(&Value::Num(-6.5)));
        assert_eq!(a[0].get("path").and_then(Value::as_str), Some("/strips/1/gain_db"));
        assert_eq!(a[1].get("x"), Some(&Value::Arr(vec![Value::Bool(true), Value::Bool(false), Value::Null])));
        assert_eq!(parse("{}").unwrap(), Value::Obj(vec![]));
        assert_eq!(parse("[]").unwrap(), Value::Arr(vec![]));
        assert_eq!(parse("1e3").unwrap(), Value::Num(1000.0));
    }

    #[test]
    fn strings_and_escapes() {
        assert_eq!(parse(r#""a\"b\\c\/\né🍌""#).unwrap(), Value::Str("a\"b\\c/\né🍌".into()));
        assert_eq!(parse("\"één\"").unwrap(), Value::Str("één".into()));
        assert!(parse("\"a\u{1}b\"").is_err());
        assert!(parse(r#""\ud83cA""#).is_err());
        assert!(parse(r#""\x""#).is_err());
        assert!(parse("\"open").is_err());
    }

    #[test]
    fn refuses_what_is_not_json() {
        for bad in ["", "{", "[1,]", "{\"a\"}", "{\"a\":1,}", "tru", "1 2", "{a:1}", "--1", "1e999", "nan", "[1}"] {
            assert!(parse(bad).is_err(), "{bad:?} parsed");
        }
        let deep = "[".repeat(MAX_DEPTH + 1) + &"]".repeat(MAX_DEPTH + 1);
        assert!(parse(&deep).unwrap_err().contains("too deep"));
        let ok = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
        assert!(parse(&ok).is_ok());
    }

    #[test]
    fn a_repeated_field_keeps_the_first() {
        assert_eq!(parse(r#"{"a":1,"a":2}"#).unwrap().get("a"), Some(&Value::Num(1.0)));
    }

    #[test]
    fn writes_back_what_it_reads() {
        let src = r#"{"a":[1,-2.5,true,null],"b":"x\"y","c":{}}"#;
        let mut o = String::new();
        write(&mut o, &parse(src).unwrap());
        assert_eq!(o, src);
    }
}
