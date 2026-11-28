//! Strict JSON parsing with the Python work modules' contract:
//! duplicate object keys are rejected, the non-finite constants
//! NaN / Infinity / -Infinity are rejected (Python's json accepts
//! them unless parse_constant refuses, which every work module
//! does), and nesting is depth-bounded (Python raises
//! RecursionError, which the callers map to their parse errors).

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrictJsonError {
    DuplicateKey,
    Constant,
    Syntax,
    Depth,
}

const MAX_DEPTH: usize = 512;

pub fn parse_strict(text: &str) -> Result<serde_json::Value, StrictJsonError> {
    let bytes = text.as_bytes();
    let mut parser = Parser { bytes, pos: 0 };
    parser.skip_ws();
    let value = parser.value(0)?;
    parser.skip_ws();
    if parser.pos != bytes.len() {
        return Err(StrictJsonError::Syntax);
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len()
            && matches!(self.bytes[self.pos], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn value(&mut self, depth: usize) -> Result<serde_json::Value, StrictJsonError> {
        if depth > MAX_DEPTH {
            return Err(StrictJsonError::Depth);
        }
        match self.peek().ok_or(StrictJsonError::Syntax)? {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => Ok(serde_json::Value::String(self.string()?)),
            b't' => self.literal("true", serde_json::Value::Bool(true)),
            b'f' => self.literal("false", serde_json::Value::Bool(false)),
            b'n' => {
                if self.bytes[self.pos..].starts_with(b"null") {
                    self.pos += 4;
                    Ok(serde_json::Value::Null)
                } else if self.bytes[self.pos..].starts_with(b"NaN") {
                    Err(StrictJsonError::Constant)
                } else {
                    Err(StrictJsonError::Syntax)
                }
            }
            b'I' => {
                if self.bytes[self.pos..].starts_with(b"Infinity") {
                    Err(StrictJsonError::Constant)
                } else {
                    Err(StrictJsonError::Syntax)
                }
            }
            b'-' => {
                if self.bytes[self.pos..].starts_with(b"-Infinity") {
                    Err(StrictJsonError::Constant)
                } else {
                    self.number()
                }
            }
            b'0'..=b'9' => self.number(),
            _ => Err(StrictJsonError::Syntax),
        }
    }

    fn literal(
        &mut self,
        text: &str,
        value: serde_json::Value,
    ) -> Result<serde_json::Value, StrictJsonError> {
        if self.bytes[self.pos..].starts_with(text.as_bytes()) {
            self.pos += text.len();
            Ok(value)
        } else {
            Err(StrictJsonError::Syntax)
        }
    }

    fn object(&mut self, depth: usize) -> Result<serde_json::Value, StrictJsonError> {
        self.pos += 1;
        let mut map = serde_json::Map::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(serde_json::Value::Object(map));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(StrictJsonError::Syntax);
            }
            let key = self.string()?;
            if map.contains_key(&key) {
                return Err(StrictJsonError::DuplicateKey);
            }
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(StrictJsonError::Syntax);
            }
            self.pos += 1;
            self.skip_ws();
            let item = self.value(depth + 1)?;
            map.insert(key, item);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(serde_json::Value::Object(map));
                }
                _ => return Err(StrictJsonError::Syntax),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<serde_json::Value, StrictJsonError> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(serde_json::Value::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value(depth + 1)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(serde_json::Value::Array(items));
                }
                _ => return Err(StrictJsonError::Syntax),
            }
        }
    }

    fn string(&mut self) -> Result<String, StrictJsonError> {
        // Delegate the token to serde_json for exact escape and
        // surrogate handling: find the closing quote first.
        let start = self.pos;
        let mut index = start + 1;
        while index < self.bytes.len() {
            match self.bytes[index] {
                b'\\' => index += 2,
                b'"' => {
                    let slice = std::str::from_utf8(&self.bytes[start..=index])
                        .map_err(|_| StrictJsonError::Syntax)?;
                    let value: String =
                        serde_json::from_str(slice).map_err(|_| StrictJsonError::Syntax)?;
                    self.pos = index + 1;
                    return Ok(value);
                }
                0x00..=0x1f => return Err(StrictJsonError::Syntax),
                _ => index += 1,
            }
        }
        Err(StrictJsonError::Syntax)
    }

    fn number(&mut self) -> Result<serde_json::Value, StrictJsonError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(StrictJsonError::Syntax),
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(StrictJsonError::Syntax);
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(StrictJsonError::Syntax);
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| StrictJsonError::Syntax)?;
        if !is_float {
            if let Ok(int) = text.parse::<i64>() {
                return Ok(serde_json::Value::Number(int.into()));
            }
            if let Ok(int) = text.parse::<u64>() {
                return Ok(serde_json::Value::Number(int.into()));
            }
            // Python parses arbitrary-precision ints; the work
            // validators bound every int well below 2^64, so an
            // overflowing literal is a syntax-class failure here.
            return Err(StrictJsonError::Syntax);
        }
        let float: f64 = text.parse().map_err(|_| StrictJsonError::Syntax)?;
        if !float.is_finite() {
            return Err(StrictJsonError::Constant);
        }
        serde_json::Number::from_f64(float)
            .map(serde_json::Value::Number)
            .ok_or(StrictJsonError::Constant)
    }
}
