use alloc::string::String;
use alloc::vec::Vec;

use crate::s;

/* A JSON value as an edge.json or an edge.lock holds one, numbers and booleans refused so a typo never passes silently. */
pub enum Value {
    Str(String),
    List(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

/* One whole document named `what` in its errors, anything past the value refused. */
pub fn parse(bytes: &[u8], what: &str) -> Result<Value, String> {
    let src = core::str::from_utf8(bytes).map_err(|_| s!(str what, " is not valid UTF-8"))?;
    let mut r = Reader { src, pos: 0, what };
    r.skip_ws();
    let value = r.value()?;
    r.skip_ws();
    if r.pos != src.len() {
        return Err(s!("unexpected text after the value in ", str what));
    }
    Ok(value)
}

/* Writes `s` as a JSON string literal, quotes included. */
pub fn quote(out: &mut String, s: &str) {
    out.push('"');
    crate::util::jesc::escape(out, s);
    out.push('"');
}

struct Reader<'a> {
    src: &'a str,
    pos: usize,
    what: &'a str,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, c: u8, msg: &str) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.pos += 1;
            return Ok(());
        }
        Err(s!(str msg, " in ", str self.what))
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b'[') => self.list(),
            Some(b'{') => self.object(),
            _ => Err(s!("unsupported value in ", str self.what, " (only strings, lists of strings and string-objects)")),
        }
    }

    // Plain runs are copied as the UTF-8 they are, only an escape is decoded byte by byte.
    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"', "expected '\"' starting a string")?;
        let mut out = String::new();
        let mut run = self.pos;
        loop {
            match self.peek() {
                None => return Err(s!("unterminated string in ", str self.what)),
                Some(b'"') => {
                    out.push_str(&self.src[run..self.pos]);
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    out.push_str(&self.src[run..self.pos]);
                    self.pos += 1;
                    let esc = self.peek().ok_or_else(|| s!("dangling '\\' in ", str self.what))?;
                    out.push(match esc {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        _ => return Err(s!("unsupported escape '\\", char esc as char, "' in ", str self.what)),
                    });
                    self.pos += 1;
                    run = self.pos;
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    fn list(&mut self) -> Result<Value, String> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::List(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::List(items));
                }
                _ => return Err(s!("expected ',' or ']' in a list in ", str self.what)),
            }
        }
    }

    fn object(&mut self) -> Result<Value, String> {
        self.pos += 1;
        let mut fields = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Obj(fields));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            self.expect(b':', "expected ':' after a key")?;
            self.skip_ws();
            let value = self.value()?;
            // A repeated key keeps its last value, the way a JSON reader in any host would.
            fields.retain(|(k, _): &(String, Value)| *k != key);
            fields.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Obj(fields));
                }
                _ => return Err(s!("expected ',' or '}' in ", str self.what)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_survives_and_escapes_decode() {
        let v = parse("{ \"d\": \"Café \\\"fuerte\\\"\" }".as_bytes(), "edge.json").unwrap();
        assert_eq!(v.get("d").and_then(Value::as_str), Some("Café \"fuerte\""));
    }

    #[test]
    fn numbers_booleans_and_trailing_text_are_refused() {
        for bad in ["{ \"a\": 1 }", "{ \"a\": true }", "{ \"a\": null }", "{} {}", "{ \"a\": \"\\u0041\" }"] {
            assert!(parse(bad.as_bytes(), "edge.json").is_err(), "{bad}");
        }
    }
}
