//! A minimal JSON READER — the other half of [`crate::json`], for the one
//! place amont must read JSON it did not write: npm's lockfiles, to tell
//! whether the installed tree a tree gate would lint with is the locked one
//! (ADR-0024). Dependency-free for the same reason `json.rs` is.
//!
//! Recursive descent over the RFC 8259 grammar. Numbers are kept as text:
//! nothing here does arithmetic on them. Depth is bounded, so a hostile file
//! cannot overflow the stack; a parse error is `None`, which every caller
//! reads as "cannot tell", the fail-closed answer.

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

const MAX_DEPTH: usize = 256;

impl Value {
    /// The member `key` of an object.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn is_true(&self) -> bool {
        matches!(self, Value::Bool(true))
    }

    pub fn members(&self) -> &[(String, Value)] {
        match self {
            Value::Obj(m) => m,
            _ => &[],
        }
    }

    pub fn items(&self) -> &[Value] {
        match self {
            Value::Arr(a) => a,
            _ => &[],
        }
    }
}

pub fn parse(text: &str) -> Option<Value> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
    };
    let v = p.value(0)?;
    p.ws();
    (p.i == p.s.len()).then_some(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, lit: &[u8]) -> Option<()> {
        if self.s.get(self.i..self.i + lit.len())? == lit {
            self.i += lit.len();
            Some(())
        } else {
            None
        }
    }

    fn value(&mut self, depth: usize) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.ws();
        match *self.s.get(self.i)? {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => self.string().map(Value::Str),
            b't' => self.eat(b"true").map(|_| Value::Bool(true)),
            b'f' => self.eat(b"false").map(|_| Value::Bool(false)),
            b'n' => self.eat(b"null").map(|_| Value::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn object(&mut self, depth: usize) -> Option<Value> {
        self.i += 1;
        let mut members = Vec::new();
        self.ws();
        if self.s.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Some(Value::Obj(members));
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            self.eat(b":")?;
            let v = self.value(depth + 1)?;
            members.push((key, v));
            self.ws();
            match *self.s.get(self.i)? {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    return Some(Value::Obj(members));
                }
                _ => return None,
            }
        }
    }

    fn array(&mut self, depth: usize) -> Option<Value> {
        self.i += 1;
        let mut items = Vec::new();
        self.ws();
        if self.s.get(self.i) == Some(&b']') {
            self.i += 1;
            return Some(Value::Arr(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            self.ws();
            match *self.s.get(self.i)? {
                b',' => self.i += 1,
                b']' => {
                    self.i += 1;
                    return Some(Value::Arr(items));
                }
                _ => return None,
            }
        }
    }

    fn number(&mut self) -> Option<Value> {
        let start = self.i;
        while self.i < self.s.len()
            && matches!(
                self.s[self.i],
                b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
            )
        {
            self.i += 1;
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).ok()?;
        text.parse::<f64>().ok()?;
        Some(Value::Num(text.to_string()))
    }

    fn string(&mut self) -> Option<String> {
        if self.s.get(self.i) != Some(&b'"') {
            return None;
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.s.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = *self.s.get(self.i)?;
                    self.i += 1;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let hex = std::str::from_utf8(self.s.get(self.i..self.i + 4)?).ok()?;
                            self.i += 4;
                            let mut code = u32::from_str_radix(hex, 16).ok()?;
                            // A surrogate pair spells one scalar value.
                            if (0xD800..0xDC00).contains(&code) {
                                self.eat(b"\\u")?;
                                let lo =
                                    std::str::from_utf8(self.s.get(self.i..self.i + 4)?).ok()?;
                                self.i += 4;
                                let lo = u32::from_str_radix(lo, 16).ok()?;
                                code =
                                    0x10000 + ((code - 0xD800) << 10) + (lo.checked_sub(0xDC00)?);
                            }
                            let ch = char::from_u32(code)?;
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return None,
                    }
                }
                c if c < 0x20 => return None,
                c => out.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_read_parses_a_lockfile_shape() {
        let v = parse(
            r#"{"name":"x","lockfileVersion":3,"packages":{"":{"name":"x"},
               "node_modules/a":{"version":"1.0.0","integrity":"sha512-AA==","dev":true},
               "node_modules/@esbuild/darwin-arm64":{"version":"0.2.0","optional":true,"os":["darwin"],"cpu":["arm64"]}}}"#,
        )
        .expect("parses");
        let pk = v.get("packages").unwrap();
        assert_eq!(pk.members().len(), 3);
        let a = pk.get("node_modules/a").unwrap();
        assert_eq!(a.get("version").and_then(Value::as_str), Some("1.0.0"));
        assert!(a.get("dev").unwrap().is_true());
        let e = pk.get("node_modules/@esbuild/darwin-arm64").unwrap();
        assert_eq!(e.get("os").unwrap().items()[0].as_str(), Some("darwin"));
    }

    #[test]
    fn json_read_escapes_and_unicode() {
        let v = parse(r#"["a\"b\\c\n", "é😀", -1.5e3, null, false]"#).unwrap();
        let items = v.items();
        assert_eq!(items[0].as_str(), Some("a\"b\\c\n"));
        assert_eq!(items[1].as_str(), Some("é😀"));
        assert_eq!(items[2], Value::Num("-1.5e3".into()));
        assert_eq!(items[3], Value::Null);
    }

    #[test]
    fn json_read_refuses_garbage_and_depth_bombs() {
        assert!(parse("{\"a\":}").is_none());
        assert!(parse("[1,2] trailing").is_none());
        assert!(parse(&"[".repeat(10_000)).is_none());
    }
}
