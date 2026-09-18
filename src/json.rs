use serde_json::{Map, Number, Value as Json};
use std::collections::HashSet;

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub max_bytes: usize,
    pub max_depth: usize,
    pub max_nodes: usize,
    pub reject_duplicate_keys: bool,
}

pub(crate) fn parse(text: &str, limits: Limits) -> Result<Json, String> {
    if text.len() > limits.max_bytes {
        return Err(format!("JSON payload exceeds {} bytes", limits.max_bytes));
    }
    let mut parser = Parser {
        text,
        at: 0,
        nodes: 0,
        limits,
    };
    let value = parser.value(0)?;
    parser.skip_ws();
    if parser.at != text.len() {
        return Err(parser.error("trailing characters after JSON value"));
    }
    Ok(value)
}

struct Parser<'a> {
    text: &'a str,
    at: usize,
    nodes: usize,
    limits: Limits,
}

impl Parser<'_> {
    fn error(&self, message: impl Into<String>) -> String {
        format!("JSON decode failed at byte {}: {}", self.at, message.into())
    }

    fn skip_ws(&mut self) {
        while matches!(
            self.text.as_bytes().get(self.at),
            Some(b' ' | b'\t' | b'\r' | b'\n')
        ) {
            self.at += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        self.skip_ws();
        self.nodes += 1;
        if self.nodes > self.limits.max_nodes {
            return Err(self.error(format!("JSON nodes exceed {}", self.limits.max_nodes)));
        }
        match self.text.as_bytes().get(self.at).copied() {
            Some(b'{') => {
                if depth >= self.limits.max_depth {
                    return Err(
                        self.error(format!("JSON nesting exceeds {}", self.limits.max_depth))
                    );
                }
                self.object(depth + 1)
            }
            Some(b'[') => {
                if depth >= self.limits.max_depth {
                    return Err(
                        self.error(format!("JSON nesting exceeds {}", self.limits.max_depth))
                    );
                }
                self.array(depth + 1)
            }
            Some(b'\"') => Ok(Json::String(self.string()?)),
            Some(b't') => {
                self.literal("true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.literal("false")?;
                Ok(Json::Bool(false))
            }
            Some(b'n') => {
                self.literal("null")?;
                Ok(Json::Null)
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.error("expected a JSON value")),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        let start = self.at;
        self.at += 1;
        while let Some(byte) = self.text.as_bytes().get(self.at).copied() {
            match byte {
                b'\"' => {
                    self.at += 1;
                    return serde_json::from_str(&self.text[start..self.at])
                        .map_err(|error| self.error(error.to_string()));
                }
                b'\\' => self.at = (self.at + 2).min(self.text.len()),
                0..=0x1f => return Err(self.error("unescaped control character in string")),
                _ => self.at += 1,
            }
        }
        Err(self.error("unterminated string"))
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.at;
        let bytes = self.text.as_bytes();
        if bytes.get(self.at) == Some(&b'-') {
            self.at += 1;
        }
        match bytes.get(self.at).copied() {
            Some(b'0') => {
                self.at += 1;
                if matches!(bytes.get(self.at), Some(b'0'..=b'9')) {
                    return Err(self.error("invalid number integer part"));
                }
            }
            Some(b'1'..=b'9') => {
                self.at += 1;
                while matches!(bytes.get(self.at), Some(b'0'..=b'9')) {
                    self.at += 1;
                }
            }
            _ => return Err(self.error("invalid number integer part")),
        }
        if bytes.get(self.at) == Some(&b'.') {
            self.at += 1;
            if !matches!(bytes.get(self.at), Some(b'0'..=b'9')) {
                return Err(self.error("fraction requires at least one digit"));
            }
            while matches!(bytes.get(self.at), Some(b'0'..=b'9')) {
                self.at += 1;
            }
        }
        if matches!(bytes.get(self.at), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(bytes.get(self.at), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if !matches!(bytes.get(self.at), Some(b'0'..=b'9')) {
                return Err(self.error("exponent requires at least one digit"));
            }
            while matches!(bytes.get(self.at), Some(b'0'..=b'9')) {
                self.at += 1;
            }
        }
        Ok(Json::Number(Number::from_string_unchecked(
            self.text[start..self.at].to_owned(),
        )))
    }

    fn array(&mut self, depth: usize) -> Result<Json, String> {
        self.at += 1;
        self.skip_ws();
        let mut values = Vec::new();
        if self.text.as_bytes().get(self.at) == Some(&b']') {
            self.at += 1;
            return Ok(Json::Array(values));
        }
        loop {
            values.push(self.value(depth)?);
            self.skip_ws();
            match self.text.as_bytes().get(self.at) {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Json::Array(values));
                }
                _ => return Err(self.error("expected ',' or ']'")),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, String> {
        self.at += 1;
        self.skip_ws();
        let mut values = Map::new();
        let mut keys = HashSet::new();
        if self.text.as_bytes().get(self.at) == Some(&b'}') {
            self.at += 1;
            return Ok(Json::Object(values));
        }
        loop {
            self.skip_ws();
            if self.text.as_bytes().get(self.at) != Some(&b'\"') {
                return Err(self.error("object key must be a string"));
            }
            let key = self.string()?;
            if self.limits.reject_duplicate_keys && !keys.insert(key.clone()) {
                return Err(self.error(format!("duplicate object key '{key}'")));
            }
            self.skip_ws();
            if self.text.as_bytes().get(self.at) != Some(&b':') {
                return Err(self.error("expected ':' after object key"));
            }
            self.at += 1;
            values.insert(key, self.value(depth)?);
            self.skip_ws();
            match self.text.as_bytes().get(self.at) {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Json::Object(values));
                }
                _ => return Err(self.error("expected ',' or '}'")),
            }
        }
    }

    fn literal(&mut self, literal: &str) -> Result<(), String> {
        if self.text[self.at..].starts_with(literal) {
            self.at += literal.len();
            Ok(())
        } else {
            Err(self.error(format!("expected '{literal}'")))
        }
    }
}
