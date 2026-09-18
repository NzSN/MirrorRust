//! Strict version-1 compiled model-interface negotiation.
//!
//! This module is intentionally independent of MirrorGate.  A sandbox facade is
//! one possible deferred adapter factory; ordinary in-process adapters use the
//! same exact-match and lifetime rules.

use crate::client::run_stepping_loop;
use crate::protocol::{ApalacheConfig, ApalacheSpec, ClientMessage, State, TraceGenerationConfig};
use crate::transport::{spawn_mirror, Transport, MAX_PROTOCOL_LINE_BYTES};
use crate::{encode_client_message, Error};
use serde::de::{DeserializeSeed, Error as DeError, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value as Json};
use std::collections::HashSet;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};

pub const MODEL_INTERFACE_CONTRACT_SCHEMA: &str = "mirrors.model-interface/v1";
pub const MODEL_INTERFACE_NEGOTIATION_SCHEMA: &str = "mirrors.model-interface-negotiation/v1";
pub const MODEL_INTERFACE_DESCRIPTOR_SCHEMA: &str = "mirrors.model-interface-descriptor/v1";
pub const STATE_COMPUTER_CONTRACT_VERSION: &str = "mirrors.state-computer/v1";

fn mi_error(code: impl Into<String>, message: impl Into<String>) -> Error {
    Error::ModelInterface {
        code: code.into(),
        message: message.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingError {
    pub code: String,
    pub message: String,
}

impl BindingError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for BindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for BindingError {}

pub trait FallibleStateComputer {
    fn compute(
        &mut self,
        action: &str,
        params: &State,
        prev: &State,
    ) -> Result<State, BindingError>;
}

impl<F> FallibleStateComputer for F
where
    F: FnMut(&str, &State, &State) -> Result<State, BindingError>,
{
    fn compute(
        &mut self,
        action: &str,
        params: &State,
        prev: &State,
    ) -> Result<State, BindingError> {
        self(action, params, prev)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SemanticDigest([u8; 32]);

impl SemanticDigest {
    pub fn from_hex(value: &str) -> Result<Self, Error> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(mi_error(
                "descriptor_digest_invalid",
                "semantic digest must contain 64 lowercase hexadecimal characters",
            ));
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let nibble = |byte: u8| {
                if byte <= b'9' {
                    byte - b'0'
                } else {
                    byte - b'a' + 10
                }
            };
            bytes[index] = (nibble(pair[0]) << 4) | nibble(pair[1]);
        }
        Ok(Self(bytes))
    }

    pub fn parse_wire(value: &str) -> Result<Self, Error> {
        Self::from_hex(value.strip_prefix("sha256:").ok_or_else(|| {
            mi_error(
                "descriptor_digest_invalid",
                "semantic digest must start with sha256:",
            )
        })?)
    }

    pub fn to_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(64);
        for byte in self.0 {
            output.push(HEX[(byte >> 4) as usize] as char);
            output.push(HEX[(byte & 0xf) as usize] as char);
        }
        output
    }

    pub fn to_wire(self) -> String {
        format!("sha256:{}", self.to_hex())
    }
}

#[derive(Debug, Clone)]
pub struct GeneratedModelInterface {
    pub semantic_digest: String,
    pub contract_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NegotiationPolicy {
    Require,
    Prefer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelInterfaceStatus {
    Matched,
    Resolved,
    NotModified,
    Mismatch,
    Unsupported,
    Unavailable,
    TooLarge,
}

#[derive(Debug, Clone)]
pub struct ModelInterfaceVerifyRequest {
    policy: NegotiationPolicy,
    expected_semantic_digest: SemanticDigest,
    contract: Json,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CompiledAdapterKey {
    pub semantic_digest: SemanticDigest,
    pub adapter_id: String,
    pub target_profile: String,
    pub state_computer_contract_version: String,
}

/// Proof object created only after a strict successful `matched` reply.
#[derive(Debug, Clone)]
pub struct MatchedBindingContext {
    semantic_digest: SemanticDigest,
    descriptor_schema: String,
    effective_config: ApalacheConfig,
}

impl MatchedBindingContext {
    pub fn semantic_digest(&self) -> SemanticDigest {
        self.semantic_digest
    }
    pub fn descriptor_schema(&self) -> &str {
        &self.descriptor_schema
    }
    pub fn effective_config(&self) -> &ApalacheConfig {
        &self.effective_config
    }
}

pub struct LocalBinding {
    pub semantic_digest: SemanticDigest,
    pub computer: Box<dyn FallibleStateComputer>,
    pub assert_compatible_config: ConfigCompatibilityCheck,
    pub dispose: Box<dyn FnMut() -> Result<(), BindingError>>,
}

pub type ConfigCompatibilityCheck = Box<dyn FnMut(&ApalacheConfig) -> Result<(), BindingError>>;

pub type AdapterFactory =
    Box<dyn FnMut(MatchedBindingContext) -> Result<LocalBinding, BindingError>>;
pub type LegacyFallbackFactory =
    Box<dyn FnMut(&ApalacheConfig) -> Result<LocalBinding, BindingError>>;

pub struct CompiledAdapterRegistration {
    pub key: CompiledAdapterKey,
    pub factory: AdapterFactory,
}

pub struct CompiledAdapterRegistry {
    registrations: Vec<CompiledAdapterRegistration>,
}

impl CompiledAdapterRegistry {
    pub fn new(registrations: Vec<CompiledAdapterRegistration>) -> Self {
        Self { registrations }
    }

    fn resolve_index(&self, key: &CompiledAdapterKey) -> Result<usize, Error> {
        let matches: Vec<_> = self
            .registrations
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.key == *key)
            .collect();
        if matches.len() > 1 {
            return Err(mi_error(
                "adapter_ambiguous",
                format!(
                    "multiple exact adapters are registered for {}",
                    key.adapter_id
                ),
            ));
        }
        if let Some((index, _)) = matches.first() {
            return Ok(*index);
        }
        let identity = self
            .registrations
            .iter()
            .filter(|entry| {
                entry.key.semantic_digest == key.semantic_digest
                    && entry.key.adapter_id == key.adapter_id
            })
            .collect::<Vec<_>>();
        if !identity.is_empty()
            && identity
                .iter()
                .all(|entry| entry.key.target_profile != key.target_profile)
        {
            return Err(mi_error(
                "target_profile_mismatch",
                format!(
                    "adapter target profile does not match {}",
                    key.target_profile
                ),
            ));
        }
        if identity
            .iter()
            .any(|entry| entry.key.target_profile == key.target_profile)
        {
            return Err(mi_error(
                "state_computer_contract_mismatch",
                format!(
                    "adapter StateComputer contract does not match {}",
                    key.state_computer_contract_version
                ),
            ));
        }
        Err(mi_error(
            "adapter_not_registered",
            format!("adapter is not registered: {}", key.adapter_id),
        ))
    }
}

pub struct CompiledAdapterSelection<'a> {
    pub metadata: GeneratedModelInterface,
    pub adapter_id: String,
    pub target_profile: String,
    pub state_computer_contract_version: String,
    pub registry: &'a mut CompiledAdapterRegistry,
    pub policy: NegotiationPolicy,
    pub fallback_factory: Option<LegacyFallbackFactory>,
}

// Parse from lexical JSON rather than through `serde_json::Value`. With
// `arbitrary_precision`, serde represents large numbers through a private map
// token which is indistinguishable from a real object to a generic visitor.
// Keeping the lexical distinction preserves ordinary objects whose key happens
// to be `$serde_json::private::Number` while retaining every JSON number.
#[allow(dead_code)]
struct RawStrictParser<'a> {
    text: &'a str,
    at: usize,
    nodes: usize,
}

#[allow(dead_code)]
impl RawStrictParser<'_> {
    fn error(&self, message: impl Into<String>) -> Error {
        mi_error(
            "negotiation_status_unexpected",
            format!(
                "strict JSON decode failed at byte {}: {}",
                self.at,
                message.into()
            ),
        )
    }

    fn skip_ws(&mut self) {
        while matches!(
            self.text.as_bytes().get(self.at),
            Some(b' ' | b'\t' | b'\r' | b'\n')
        ) {
            self.at += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, Error> {
        self.skip_ws();
        self.nodes += 1;
        if self.nodes > 16_384 {
            return Err(self.error("JSON nodes exceed 16384"));
        }
        match self.text.as_bytes().get(self.at).copied() {
            Some(b'{') => {
                if depth >= 128 {
                    return Err(self.error("JSON nesting exceeds 128"));
                }
                self.object(depth + 1)
            }
            Some(b'[') => {
                if depth >= 128 {
                    return Err(self.error("JSON nesting exceeds 128"));
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

    fn string(&mut self) -> Result<String, Error> {
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

    fn number(&mut self) -> Result<Json, Error> {
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

    fn array(&mut self, depth: usize) -> Result<Json, Error> {
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

    fn object(&mut self, depth: usize) -> Result<Json, Error> {
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
            if !keys.insert(key.clone()) {
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

    fn literal(&mut self, literal: &str) -> Result<(), Error> {
        if self.text[self.at..].starts_with(literal) {
            self.at += literal.len();
            Ok(())
        } else {
            Err(self.error(format!("expected '{literal}'")))
        }
    }
}

#[derive(Default)]
#[allow(dead_code)]
struct StrictState {
    nodes: usize,
}

#[allow(dead_code)]
struct StrictSeed<'a> {
    state: &'a mut StrictState,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for StrictSeed<'_> {
    type Value = Json;
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<Json, D::Error> {
        if self.depth > 128 {
            return Err(D::Error::custom("JSON nesting exceeds 128"));
        }
        self.state.nodes += 1;
        if self.state.nodes > 16_384 {
            return Err(D::Error::custom("JSON nodes exceed 16384"));
        }
        deserializer.deserialize_any(StrictVisitor {
            state: self.state,
            depth: self.depth,
        })
    }
}

#[allow(dead_code)]
struct StrictVisitor<'a> {
    state: &'a mut StrictState,
    depth: usize,
}

impl<'de> Visitor<'de> for StrictVisitor<'_> {
    type Value = Json;
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON value")
    }
    fn visit_bool<E: DeError>(self, value: bool) -> Result<Json, E> {
        Ok(Json::Bool(value))
    }
    fn visit_i64<E: DeError>(self, value: i64) -> Result<Json, E> {
        Ok(Json::Number(value.into()))
    }
    fn visit_u64<E: DeError>(self, value: u64) -> Result<Json, E> {
        Ok(Json::Number(value.into()))
    }
    fn visit_f64<E: DeError>(self, value: f64) -> Result<Json, E> {
        Number::from_f64(value)
            .map(Json::Number)
            .ok_or_else(|| E::custom("non-finite number"))
    }
    fn visit_str<E: DeError>(self, value: &str) -> Result<Json, E> {
        Ok(Json::String(value.to_owned()))
    }
    fn visit_string<E: DeError>(self, value: String) -> Result<Json, E> {
        Ok(Json::String(value))
    }
    fn visit_none<E: DeError>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }
    fn visit_unit<E: DeError>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }
    fn visit_some<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<Json, D::Error> {
        StrictSeed {
            state: self.state,
            depth: self.depth,
        }
        .deserialize(deserializer)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Json, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(StrictSeed {
            state: self.state,
            depth: self.depth + 1,
        })? {
            values.push(value);
        }
        Ok(Json::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Json, A::Error> {
        let mut values = Map::new();
        let mut keys = HashSet::new();
        while let Some(key) = object.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(A::Error::custom(format!("duplicate object key '{key}'")));
            }
            let value = object.next_value_seed(StrictSeed {
                state: self.state,
                depth: self.depth + 1,
            })?;
            values.insert(key, value);
        }
        Ok(Json::Object(values))
    }
}

fn strict_parse(text: &str) -> Result<Json, Error> {
    crate::json::parse(
        text,
        crate::json::Limits {
            max_bytes: MAX_PROTOCOL_LINE_BYTES,
            max_depth: 128,
            max_nodes: 16_384,
            reject_duplicate_keys: true,
        },
    )
    .map_err(|error| mi_error("negotiation_status_unexpected", error))
}

#[allow(dead_code)]
fn validate_number_tokens(text: &str) -> Result<(), Error> {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'"' {
            at += 1;
            while at < bytes.len() {
                match bytes[at] {
                    b'\\' => at = (at + 2).min(bytes.len()),
                    b'"' => {
                        at += 1;
                        break;
                    }
                    _ => at += 1,
                }
            }
            continue;
        }
        if bytes[at] == b'-' || bytes[at].is_ascii_digit() {
            let start = at;
            at += 1;
            while at < bytes.len()
                && !matches!(
                    bytes[at],
                    b' ' | b'\t' | b'\r' | b'\n' | b',' | b']' | b'}' | b':'
                )
            {
                at += 1;
            }
            let token = &text[start..at];
            let digits = token.strip_prefix('-').unwrap_or(token);
            let canonical = !digits.is_empty()
                && digits.bytes().all(|byte| byte.is_ascii_digit())
                && (digits == "0" || !digits.starts_with('0'));
            let representable = if token.starts_with('-') {
                token.parse::<i64>().is_ok()
            } else {
                token.parse::<u64>().is_ok()
            };
            if !canonical || !representable {
                return Err(mi_error(
                    "negotiation_status_unexpected",
                    "strict JSON contains a non-integral or out-of-range number",
                ));
            }
            continue;
        }
        at += 1;
    }
    Ok(())
}

fn object<'a>(
    value: &'a Json,
    path: &str,
    allowed: &[&str],
    required: &[&str],
) -> Result<&'a Map<String, Json>, Error> {
    let map = value.as_object().ok_or_else(|| {
        mi_error(
            "negotiation_status_unexpected",
            format!("{path}: object expected"),
        )
    })?;
    if let Some(key) = map.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(mi_error(
            "negotiation_status_unexpected",
            format!("{path}: unknown field '{key}'"),
        ));
    }
    if let Some(key) = required.iter().find(|key| !map.contains_key(**key)) {
        return Err(mi_error(
            "negotiation_status_unexpected",
            format!("{path}: missing required field '{key}'"),
        ));
    }
    Ok(map)
}

fn short_string<'a>(value: &'a Json, path: &str) -> Result<&'a str, Error> {
    let text = value.as_str().ok_or_else(|| {
        mi_error(
            "negotiation_status_unexpected",
            format!("{path}: string expected"),
        )
    })?;
    if text.len() > 256 {
        return Err(mi_error(
            "negotiation_status_unexpected",
            format!("{path}: exceeds 256 UTF-8 bytes"),
        ));
    }
    Ok(text)
}

fn canonical_decimal(value: &str) -> bool {
    let digits = value.strip_prefix('-').unwrap_or(value);
    !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'))
        && value != "-0"
}

fn validate_itf_literal(
    value: &Json,
    depth: usize,
    nodes: &mut usize,
    path: &str,
) -> Result<(), Error> {
    if depth >= 128 {
        return Err(mi_error(
            "negotiation_status_unexpected",
            format!("{path}: canonical ITF literal nesting exceeds 128"),
        ));
    }
    *nodes += 1;
    if *nodes > 16_384 {
        return Err(mi_error(
            "negotiation_status_unexpected",
            format!("{path}: canonical ITF literal nodes exceed 16384"),
        ));
    }
    let initial = value.as_object().ok_or_else(|| {
        mi_error(
            "negotiation_status_unexpected",
            format!("{path}: canonical ITF literal object expected"),
        )
    })?;
    let kind = initial.get("kind").and_then(Json::as_str).ok_or_else(|| {
        mi_error(
            "negotiation_status_unexpected",
            format!("{path}: canonical ITF literal kind expected"),
        )
    })?;
    match kind {
        "int" => {
            let fields = object(value, path, &["kind", "value"], &["kind", "value"])?;
            let integer = short_string(&fields["value"], path)?;
            if !canonical_decimal(integer) {
                return Err(mi_error(
                    "negotiation_status_unexpected",
                    format!("{path}: canonical decimal integer string expected"),
                ));
            }
        }
        "bool" => {
            let fields = object(value, path, &["kind", "value"], &["kind", "value"])?;
            if !fields["value"].is_boolean() {
                return Err(mi_error(
                    "negotiation_status_unexpected",
                    format!("{path}: Boolean expected"),
                ));
            }
        }
        "str" => {
            let fields = object(value, path, &["kind", "value"], &["kind", "value"])?;
            short_string(&fields["value"], path)?;
        }
        "null" => {
            object(value, path, &["kind"], &["kind"])?;
        }
        "set" | "seq" | "tuple" => {
            let fields = object(value, path, &["kind", "values"], &["kind", "values"])?;
            let values = fields["values"].as_array().ok_or_else(|| {
                mi_error(
                    "negotiation_status_unexpected",
                    format!("{path}: values array expected"),
                )
            })?;
            for item in values {
                validate_itf_literal(item, depth + 1, nodes, path)?;
            }
        }
        "record" => {
            let fields = object(value, path, &["kind", "fields"], &["kind", "fields"])?;
            let entries = fields["fields"].as_array().ok_or_else(|| {
                mi_error(
                    "negotiation_status_unexpected",
                    format!("{path}: fields array expected"),
                )
            })?;
            for entry in entries {
                let entry = object(entry, path, &["name", "value"], &["name", "value"])?;
                short_string(&entry["name"], path)?;
                validate_itf_literal(&entry["value"], depth + 1, nodes, path)?;
            }
        }
        "map" => {
            let fields = object(value, path, &["kind", "entries"], &["kind", "entries"])?;
            let entries = fields["entries"].as_array().ok_or_else(|| {
                mi_error(
                    "negotiation_status_unexpected",
                    format!("{path}: entries array expected"),
                )
            })?;
            for entry in entries {
                let entry = object(entry, path, &["key", "value"], &["key", "value"])?;
                validate_itf_literal(&entry["key"], depth + 1, nodes, path)?;
                validate_itf_literal(&entry["value"], depth + 1, nodes, path)?;
            }
        }
        "variant" => {
            let fields = object(
                value,
                path,
                &["kind", "tag", "payload"],
                &["kind", "tag", "payload"],
            )?;
            short_string(&fields["tag"], path)?;
            validate_itf_literal(&fields["payload"], depth + 1, nodes, path)?;
        }
        _ => {
            return Err(mi_error(
                "negotiation_status_unexpected",
                format!("{path}: unknown canonical ITF literal '{kind}'"),
            ));
        }
    }
    Ok(())
}

fn validate_path(value: &Json, path: &str, nodes: &mut usize) -> Result<(), Error> {
    let fields = value
        .as_object()
        .filter(|value| value.len() == 1)
        .ok_or_else(|| {
            mi_error(
                "negotiation_status_unexpected",
                format!("{path}: exactly one selector required"),
            )
        })?;
    let (kind, value) = fields.iter().next().unwrap();
    match kind.as_str() {
        "field" | "variantValue" => {
            short_string(value, path)?;
            Ok(())
        }
        "index" if value.as_u64().is_some() => Ok(()),
        "mapKey" => validate_itf_literal(value, 0, nodes, path),
        _ => Err(mi_error(
            "negotiation_status_unexpected",
            format!("{path}: invalid path selector"),
        )),
    }
}

fn validate_model_type(
    value: &Json,
    depth: usize,
    nodes: &mut usize,
    path: &str,
) -> Result<(), Error> {
    if depth >= 32 {
        return Err(mi_error(
            "negotiation_status_unexpected",
            format!("{path}: type depth exceeds 32"),
        ));
    }
    *nodes += 1;
    if *nodes > 8_192 {
        return Err(mi_error(
            "negotiation_status_unexpected",
            format!("{path}: type nodes exceed 8192"),
        ));
    }
    let tagged = value.as_object().ok_or_else(|| {
        mi_error(
            "negotiation_status_unexpected",
            format!("{path}: tagged model type expected"),
        )
    })?;
    let kind = tagged.get("kind").and_then(Json::as_str).ok_or_else(|| {
        mi_error(
            "negotiation_status_unexpected",
            format!("{path}: tagged model type expected"),
        )
    })?;
    match kind {
        "int" | "bool" | "str" | "null" => {
            object(value, path, &["kind"], &["kind"])?;
        }
        "set" | "seq" => {
            let tagged = object(value, path, &["kind", "element"], &["kind", "element"])?;
            validate_model_type(&tagged["element"], depth + 1, nodes, path)?;
        }
        "tuple" => {
            let tagged = object(value, path, &["kind", "elements"], &["kind", "elements"])?;
            let elements = tagged["elements"].as_array().ok_or_else(|| {
                mi_error(
                    "negotiation_status_unexpected",
                    format!("{path}: elements array expected"),
                )
            })?;
            for element in elements {
                validate_model_type(element, depth + 1, nodes, path)?;
            }
        }
        "record" | "variant" => {
            let list_name = if kind == "record" { "fields" } else { "cases" };
            let name = if kind == "record" { "wireName" } else { "tag" };
            let payload = if kind == "record" { "type" } else { "payload" };
            let tagged = object(value, path, &["kind", list_name], &["kind", list_name])?;
            let entries = tagged[list_name].as_array().ok_or_else(|| {
                mi_error(
                    "negotiation_status_unexpected",
                    format!("{path}: {list_name} array expected"),
                )
            })?;
            let mut names = HashSet::new();
            for entry in entries {
                let entry = object(entry, path, &[name, payload], &[name, payload])?;
                let label = short_string(&entry[name], path)?;
                if label.is_empty() || !names.insert(label) {
                    return Err(mi_error(
                        "negotiation_status_unexpected",
                        format!("{path}: duplicate or empty label"),
                    ));
                }
                validate_model_type(&entry[payload], depth + 1, nodes, path)?;
            }
        }
        "map" => {
            let tagged = object(
                value,
                path,
                &["kind", "key", "value"],
                &["kind", "key", "value"],
            )?;
            validate_model_type(&tagged["key"], depth + 1, nodes, path)?;
            validate_model_type(&tagged["value"], depth + 1, nodes, path)?;
        }
        "opaqueItf" => {
            let tagged = object(
                value,
                path,
                &["kind", "description"],
                &["kind", "description"],
            )?;
            short_string(&tagged["description"], path)?;
        }
        _ => {
            return Err(mi_error(
                "negotiation_status_unexpected",
                format!("{path}: unknown model type {kind}"),
            ))
        }
    }
    Ok(())
}

fn validate_action(value: &Json, path: &str, nodes: &mut usize) -> Result<(), Error> {
    let action = object(
        value,
        path,
        &["id", "wireAction", "wireAliases", "inputs"],
        &["id", "wireAction", "wireAliases", "inputs"],
    )?;
    short_string(&action["id"], path)?;
    short_string(&action["wireAction"], path)?;
    let aliases = action["wireAliases"]
        .as_array()
        .filter(|items| items.len() <= 16)
        .ok_or_else(|| {
            mi_error(
                "negotiation_status_unexpected",
                format!("{path}: alias resource limit exceeded"),
            )
        })?;
    for alias in aliases {
        short_string(alias, path)?;
    }
    let inputs = action["inputs"]
        .as_array()
        .filter(|items| items.len() <= 128)
        .ok_or_else(|| {
            mi_error(
                "negotiation_status_unexpected",
                format!("{path}: input resource limit exceeded"),
            )
        })?;
    for input in inputs {
        let input = object(
            input,
            path,
            &["id", "from", "expectedType"],
            &["id", "from"],
        )?;
        short_string(&input["id"], path)?;
        let from = object(&input["from"], path, &["root", "path"], &["root", "path"])?;
        if !matches!(
            from["root"].as_str(),
            Some("initialState" | "stepParameters")
        ) {
            return Err(mi_error(
                "negotiation_status_unexpected",
                format!("{path}: invalid path root"),
            ));
        }
        let segments = from["path"]
            .as_array()
            .filter(|items| items.len() <= 32)
            .ok_or_else(|| {
                mi_error(
                    "negotiation_status_unexpected",
                    format!("{path}: path limit exceeded"),
                )
            })?;
        for segment in segments {
            validate_path(segment, path, nodes)?;
        }
        if let Some(expected) = input.get("expectedType").filter(|value| !value.is_null()) {
            validate_model_type(expected, 0, nodes, path)?;
        }
    }
    Ok(())
}

fn validate_contract(value: &Json) -> Result<(), Error> {
    let path = "modelInterface.contract.inline";
    let contract = object(
        value,
        path,
        &[
            "schema",
            "interfaceVersion",
            "model",
            "wire",
            "initializers",
            "actions",
            "observations",
        ],
        &[
            "schema",
            "interfaceVersion",
            "model",
            "wire",
            "initializers",
            "actions",
            "observations",
        ],
    )?;
    if contract["schema"] != MODEL_INTERFACE_CONTRACT_SCHEMA {
        return Err(mi_error(
            "negotiation_status_unexpected",
            "modelInterface contract schema is unsupported",
        ));
    }
    short_string(&contract["interfaceVersion"], path)?;
    let model = object(
        &contract["model"],
        path,
        &["module", "source"],
        &["module", "source"],
    )?;
    short_string(&model["module"], path)?;
    short_string(&model["source"], path)?;
    let wire = object(
        &contract["wire"],
        path,
        &["actionVariable", "parameterVariable"],
        &["actionVariable", "parameterVariable"],
    )?;
    short_string(&wire["actionVariable"], path)?;
    if !wire["parameterVariable"].is_null() {
        short_string(&wire["parameterVariable"], path)?;
    }
    let initializers = contract["initializers"]
        .as_array()
        .filter(|items| items.len() <= 32)
        .ok_or_else(|| {
            mi_error(
                "negotiation_status_unexpected",
                "modelInterface initializers limit exceeded",
            )
        })?;
    let actions = contract["actions"]
        .as_array()
        .filter(|items| items.len() <= 256)
        .ok_or_else(|| {
            mi_error(
                "negotiation_status_unexpected",
                "modelInterface actions limit exceeded",
            )
        })?;
    let observations = contract["observations"]
        .as_array()
        .filter(|items| items.len() <= 1024)
        .ok_or_else(|| {
            mi_error(
                "negotiation_status_unexpected",
                "modelInterface observations limit exceeded",
            )
        })?;
    let mut nodes = 0;
    for action in initializers.iter().chain(actions) {
        validate_action(action, path, &mut nodes)?;
    }
    for observation in observations {
        let observation = object(
            observation,
            path,
            &["id", "wireName", "provenance", "expectedType"],
            &["id", "wireName", "provenance"],
        )?;
        short_string(&observation["id"], path)?;
        short_string(&observation["wireName"], path)?;
        if !matches!(
            observation["provenance"].as_str(),
            Some("implementation" | "oracle" | "derived")
        ) {
            return Err(mi_error(
                "negotiation_status_unexpected",
                "invalid observation provenance",
            ));
        }
        if let Some(expected) = observation
            .get("expectedType")
            .filter(|value| !value.is_null())
        {
            validate_model_type(expected, 0, &mut nodes, path)?;
        }
    }
    Ok(())
}

pub fn make_verify_request(
    metadata: &GeneratedModelInterface,
    policy: NegotiationPolicy,
) -> Result<ModelInterfaceVerifyRequest, Error> {
    let expected_semantic_digest = SemanticDigest::from_hex(&metadata.semantic_digest)?;
    let contract = strict_parse(&metadata.contract_json)?;
    validate_contract(&contract)?;
    Ok(ModelInterfaceVerifyRequest {
        policy,
        expected_semantic_digest,
        contract,
    })
}

fn request_json(request: &ModelInterfaceVerifyRequest) -> Json {
    serde_json::json!({
        "schema": MODEL_INTERFACE_NEGOTIATION_SCHEMA,
        "request": "verify",
        "policy": if request.policy == NegotiationPolicy::Require { "require" } else { "prefer" },
        "acceptDescriptorSchemas": [MODEL_INTERFACE_DESCRIPTOR_SCHEMA],
        "expectedSemanticDigest": request.expected_semantic_digest.to_wire(),
        "contract": { "inline": request.contract }
    })
}

fn encode_registration(
    registration: ClientMessage,
    request: &ModelInterfaceVerifyRequest,
) -> Result<String, Error> {
    let mut outer: Json = serde_json::from_str(&encode_client_message(&registration))?;
    outer
        .as_object_mut()
        .unwrap()
        .insert("modelInterface".into(), request_json(request));
    let encoded = serde_json::to_string(&outer)?;
    if encoded.len() > MAX_PROTOCOL_LINE_BYTES {
        return Err(mi_error(
            "negotiation_status_unexpected",
            "model-interface registration exceeds 65535 bytes",
        ));
    }
    Ok(encoded)
}

fn status(value: &str) -> Result<ModelInterfaceStatus, Error> {
    match value {
        "matched" => Ok(ModelInterfaceStatus::Matched),
        "resolved" => Ok(ModelInterfaceStatus::Resolved),
        "not_modified" => Ok(ModelInterfaceStatus::NotModified),
        "mismatch" => Ok(ModelInterfaceStatus::Mismatch),
        "unsupported" => Ok(ModelInterfaceStatus::Unsupported),
        "unavailable" => Ok(ModelInterfaceStatus::Unavailable),
        "too_large" => Ok(ModelInterfaceStatus::TooLarge),
        _ => Err(mi_error(
            "negotiation_status_unexpected",
            format!("unknown model-interface status: {value}"),
        )),
    }
}

fn optional_digest(map: &Map<String, Json>, key: &str) -> Result<Option<SemanticDigest>, Error> {
    match map.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(value)) => Ok(Some(SemanticDigest::parse_wire(value)?)),
        _ => Err(mi_error(
            "descriptor_digest_invalid",
            format!("modelInterface.{key}: string expected"),
        )),
    }
}

fn optional_short_string<'a>(
    map: &'a Map<String, Json>,
    key: &str,
) -> Result<Option<&'a str>, Error> {
    match map.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(value) => short_string(value, &format!("modelInterface.{key}")).map(Some),
    }
}

fn optional_nonnegative_integer(map: &Map<String, Json>, key: &str) -> Result<bool, Error> {
    match map.get(key) {
        None | Some(Json::Null) => Ok(false),
        Some(Json::Number(value))
            if value.to_string().bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            Ok(true)
        }
        _ => Err(mi_error(
            "negotiation_status_unexpected",
            format!("modelInterface.{key}: nonnegative integer expected"),
        )),
    }
}

enum Authorization {
    Matched {
        digest: SemanticDigest,
        descriptor_schema: String,
    },
    Fallback,
}

enum RegistrationReply {
    RegisterError(String),
    SpecValid,
    SpecInvalid(String),
    ProtocolError(String),
    Other(String),
}

fn registration_reply(outer: &Map<String, Json>) -> Result<RegistrationReply, Error> {
    let step = outer
        .get("proto_step")
        .and_then(Json::as_str)
        .ok_or_else(|| {
            mi_error(
                "negotiation_status_unexpected",
                "registration reply proto_step must be a string",
            )
        })?;
    let string_field = |name: &str| {
        outer
            .get(name)
            .and_then(Json::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                mi_error(
                    "negotiation_status_unexpected",
                    format!("registration reply {name} must be a string"),
                )
            })
    };
    match step {
        "register_error" => Ok(RegistrationReply::RegisterError(string_field("error")?)),
        "protocol_error" => Ok(RegistrationReply::ProtocolError(string_field("error")?)),
        "spec_validated" => match outer.get("result") {
            Some(Json::String(result)) if result == "valid" => Ok(RegistrationReply::SpecValid),
            Some(Json::Object(result)) => result
                .get("invalid")
                .and_then(Json::as_str)
                .map(|detail| RegistrationReply::SpecInvalid(detail.to_owned()))
                .ok_or_else(|| {
                    mi_error(
                        "negotiation_status_unexpected",
                        "invalid spec result must contain a string",
                    )
                }),
            _ => Err(mi_error(
                "negotiation_status_unexpected",
                "spec result has an invalid shape",
            )),
        },
        other => Ok(RegistrationReply::Other(other.to_owned())),
    }
}

fn decode_authorization(
    line: &str,
    request: &ModelInterfaceVerifyRequest,
    fallback: bool,
) -> Result<Authorization, Error> {
    let raw = strict_parse(line)?;
    let outer = raw.as_object().ok_or_else(|| {
        mi_error(
            "negotiation_status_unexpected",
            "mirror registration reply must be an object",
        )
    })?;
    let message = registration_reply(outer)?;
    let extension = outer.get("modelInterface").filter(|value| !value.is_null());
    match message {
        RegistrationReply::RegisterError(error) => {
            let extension = extension.ok_or_else(|| Error::RegisterFailed(error.clone()))?;
            let failure = object(
                extension,
                "modelInterface",
                &[
                    "schema",
                    "status",
                    "code",
                    "expectedSemanticDigest",
                    "actualSemanticDigest",
                    "provenanceDigest",
                    "descriptorBytes",
                ],
                &["schema", "status", "code"],
            )?;
            if failure["schema"] != MODEL_INTERFACE_NEGOTIATION_SCHEMA {
                return Err(mi_error(
                    "negotiation_status_unexpected",
                    "unsupported model-interface failure schema",
                ));
            }
            let failure_status =
                status(short_string(&failure["status"], "modelInterface.status")?)?;
            if !matches!(
                failure_status,
                ModelInterfaceStatus::Mismatch
                    | ModelInterfaceStatus::Unsupported
                    | ModelInterfaceStatus::Unavailable
                    | ModelInterfaceStatus::TooLarge
            ) {
                return Err(mi_error(
                    "negotiation_status_unexpected",
                    "unexpected status on verify register_error",
                ));
            }
            let code = short_string(&failure["code"], "modelInterface.code")?;
            if code.is_empty() {
                return Err(mi_error(
                    "negotiation_status_unexpected",
                    "model-interface failure code is empty",
                ));
            }
            let expected = optional_digest(failure, "expectedSemanticDigest")?;
            let _actual = optional_digest(failure, "actualSemanticDigest")?;
            let _provenance = optional_digest(failure, "provenanceDigest")?;
            let has_descriptor_bytes = optional_nonnegative_integer(failure, "descriptorBytes")?;
            if expected.is_some_and(|digest| digest != request.expected_semantic_digest) {
                return Err(mi_error(
                    "interface_digest_mismatch",
                    "failure expectedSemanticDigest does not match the request pin",
                ));
            }
            if failure_status == ModelInterfaceStatus::Mismatch && expected.is_none() {
                return Err(mi_error(
                    "negotiation_status_unexpected",
                    "mismatch failure lacks expectedSemanticDigest",
                ));
            }
            if failure_status == ModelInterfaceStatus::TooLarge && !has_descriptor_bytes {
                return Err(mi_error(
                    "negotiation_status_unexpected",
                    "too_large failure lacks descriptorBytes",
                ));
            }
            Err(Error::Registration {
                code: code.to_owned(),
                message: error,
            })
        }
        RegistrationReply::SpecInvalid(detail) => Err(Error::SpecInvalid(detail)),
        RegistrationReply::SpecValid => {
            let Some(extension) = extension else {
                return if request.policy == NegotiationPolicy::Prefer && fallback {
                    Ok(Authorization::Fallback)
                } else {
                    Err(mi_error(
                        if request.policy == NegotiationPolicy::Prefer {
                            "legacy_fallback_unavailable"
                        } else {
                            "negotiation_missing"
                        },
                        "model-interface negotiation reply is missing",
                    ))
                };
            };
            let reply = object(
                extension,
                "modelInterface",
                &[
                    "schema",
                    "status",
                    "descriptorSchema",
                    "semanticDigest",
                    "provenanceDigest",
                    "descriptorBytes",
                    "descriptor",
                ],
                &["schema", "status"],
            )?;
            if reply["schema"] != MODEL_INTERFACE_NEGOTIATION_SCHEMA {
                return Err(mi_error(
                    "negotiation_status_unexpected",
                    "unsupported model-interface negotiation schema",
                ));
            }
            let descriptor_schema = optional_short_string(reply, "descriptorSchema")?;
            let semantic_digest = optional_digest(reply, "semanticDigest")?;
            let _provenance_digest = optional_digest(reply, "provenanceDigest")?;
            let has_descriptor_bytes = optional_nonnegative_integer(reply, "descriptorBytes")?;
            let has_descriptor = reply
                .get("descriptor")
                .is_some_and(|value| !value.is_null());
            match status(short_string(&reply["status"], "modelInterface.status")?)? {
                ModelInterfaceStatus::Matched => {
                    let descriptor = descriptor_schema.ok_or_else(|| {
                        mi_error(
                            "negotiation_status_unexpected",
                            "matched reply lacks descriptorSchema",
                        )
                    })?;
                    let digest = semantic_digest.ok_or_else(|| {
                        mi_error(
                            "negotiation_status_unexpected",
                            "matched reply lacks semanticDigest",
                        )
                    })?;
                    if descriptor != MODEL_INTERFACE_DESCRIPTOR_SCHEMA
                        || digest != request.expected_semantic_digest
                        || has_descriptor
                        || has_descriptor_bytes
                    {
                        return Err(mi_error(
                            "interface_digest_mismatch",
                            "invalid or mismatched compiled model-interface reply",
                        ));
                    }
                    Ok(Authorization::Matched {
                        digest,
                        descriptor_schema: descriptor.to_owned(),
                    })
                }
                ModelInterfaceStatus::Unsupported | ModelInterfaceStatus::Unavailable => {
                    if descriptor_schema.is_some()
                        || semantic_digest.is_some()
                        || _provenance_digest.is_some()
                        || has_descriptor_bytes
                        || has_descriptor
                    {
                        return Err(mi_error(
                            "negotiation_status_unexpected",
                            "descriptor identity fields are forbidden when resolution is unavailable",
                        ));
                    }
                    if request.policy == NegotiationPolicy::Prefer && fallback {
                        Ok(Authorization::Fallback)
                    } else {
                        Err(mi_error(
                            "negotiation_status_unexpected",
                            "model-interface fallback status is not permitted",
                        ))
                    }
                }
                ModelInterfaceStatus::TooLarge => {
                    if descriptor_schema != Some(MODEL_INTERFACE_DESCRIPTOR_SCHEMA)
                        || semantic_digest.is_none()
                        || has_descriptor
                        || !has_descriptor_bytes
                    {
                        return Err(mi_error(
                            "negotiation_status_unexpected",
                            "invalid too_large model-interface reply",
                        ));
                    }
                    if request.policy == NegotiationPolicy::Prefer && fallback {
                        Ok(Authorization::Fallback)
                    } else {
                        Err(mi_error(
                            "negotiation_status_unexpected",
                            "model-interface fallback status is not permitted",
                        ))
                    }
                }
                ModelInterfaceStatus::Mismatch => {
                    if has_descriptor || has_descriptor_bytes {
                        return Err(mi_error(
                            "negotiation_status_unexpected",
                            "descriptor payload is forbidden for mismatch",
                        ));
                    }
                    Err(mi_error(
                        "interface_digest_mismatch",
                        "model-interface digest mismatch",
                    ))
                }
                ModelInterfaceStatus::Resolved | ModelInterfaceStatus::NotModified => {
                    if descriptor_schema != Some(MODEL_INTERFACE_DESCRIPTOR_SCHEMA)
                        || semantic_digest.is_none()
                    {
                        return Err(mi_error(
                            "negotiation_status_unexpected",
                            "descriptor identity is required for this status",
                        ));
                    }
                    if matches!(
                        status(short_string(&reply["status"], "modelInterface.status")?)?,
                        ModelInterfaceStatus::Resolved
                    ) && (!has_descriptor || !has_descriptor_bytes)
                    {
                        return Err(mi_error(
                            "negotiation_status_unexpected",
                            "resolved reply requires descriptor and descriptorBytes",
                        ));
                    }
                    if matches!(
                        status(short_string(&reply["status"], "modelInterface.status")?)?,
                        ModelInterfaceStatus::NotModified
                    ) && (has_descriptor || has_descriptor_bytes)
                    {
                        return Err(mi_error(
                            "negotiation_status_unexpected",
                            "not_modified reply forbids descriptor payload",
                        ));
                    }
                    Err(mi_error(
                        "negotiation_status_unexpected",
                        "descriptor status is invalid for compiled verification",
                    ))
                }
            }
        }
        RegistrationReply::ProtocolError(error) => Err(Error::ProtocolError(error)),
        RegistrationReply::Other(other) => Err(mi_error(
            "negotiation_status_unexpected",
            format!("expected registration result, got {other}"),
        )),
    }
}

fn dispose(binding: &mut LocalBinding) -> Result<(), Error> {
    catch_unwind(AssertUnwindSafe(|| (binding.dispose)()))
        .map_err(|_| mi_error("adapter_dispose_failed", "binding disposal panicked"))?
        .map_err(|error| mi_error("adapter_dispose_failed", error.to_string()))
}

fn run_negotiated_open(
    transport: &mut Transport,
    config: ApalacheConfig,
    registration: ClientMessage,
    selection: &mut CompiledAdapterSelection<'_>,
) -> Result<(), Error> {
    let request = make_verify_request(&selection.metadata, selection.policy)?;
    if selection.state_computer_contract_version != STATE_COMPUTER_CONTRACT_VERSION {
        return Err(mi_error(
            "state_computer_contract_mismatch",
            "negotiated runner requires mirrors.state-computer/v1",
        ));
    }
    let key = CompiledAdapterKey {
        semantic_digest: request.expected_semantic_digest,
        adapter_id: selection.adapter_id.clone(),
        target_profile: selection.target_profile.clone(),
        state_computer_contract_version: selection.state_computer_contract_version.clone(),
    };
    let factory_index = selection.registry.resolve_index(&key)?;
    let encoded = encode_registration(registration, &request)?;
    transport.send(&encoded)?;
    let first = match transport.recv()? {
        Some(line) => line,
        None => return Err(Error::TransportClosed),
    };
    let authority = decode_authorization(&first, &request, selection.fallback_factory.is_some())?;
    let created = catch_unwind(AssertUnwindSafe(|| match authority {
        Authorization::Matched {
            digest,
            descriptor_schema,
        } => {
            let context = MatchedBindingContext {
                semantic_digest: digest,
                descriptor_schema,
                effective_config: config.clone(),
            };
            (selection.registry.registrations[factory_index].factory)(context)
        }
        Authorization::Fallback => (selection
            .fallback_factory
            .as_mut()
            .expect("authorization checked"))(&config),
    }))
    .map_err(|_| mi_error("adapter_factory_failed", "adapter factory panicked"))?;
    let mut binding = match created {
        Ok(binding) => binding,
        Err(error) => return Err(mi_error("adapter_factory_failed", error.to_string())),
    };
    let primary = catch_unwind(AssertUnwindSafe(|| {
        if binding.semantic_digest != key.semantic_digest {
            Err(mi_error(
                "binding_digest_mismatch",
                "binding digest does not match adapter key",
            ))
        } else if let Err(error) = (binding.assert_compatible_config)(&config) {
            Err(mi_error("binding_config_mismatch", error.to_string()))
        } else {
            run_stepping_loop(transport, |action, params, prev| {
                binding
                    .computer
                    .compute(action, params, prev)
                    .map_err(|error| mi_error(error.code, error.message))
            })
        }
    }))
    .unwrap_or_else(|_| Err(mi_error("adapter_failure", "binding callback panicked")));
    let cleanup = dispose(&mut binding);
    match (primary, cleanup) {
        (Err(primary), _) => Err(primary),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn run_negotiated(
    mut transport: Transport,
    config: ApalacheConfig,
    registration: ClientMessage,
    selection: &mut CompiledAdapterSelection<'_>,
) -> Result<(), Error> {
    let primary = run_negotiated_open(&mut transport, config, registration, selection);
    let close = transport.close().map(|_| ());
    match (primary, close) {
        (Err(primary), _) => Err(primary),
        (Ok(()), Err(close)) => Err(close),
        (Ok(()), Ok(())) => Ok(()),
    }
}

pub fn run_client_with_traces_negotiated(
    bin_path: &str,
    config: ApalacheConfig,
    trace_paths: Vec<String>,
    selection: &mut CompiledAdapterSelection<'_>,
) -> Result<(), Error> {
    run_client_with_traces_negotiated_transport(
        spawn_mirror(bin_path)?,
        config,
        trace_paths,
        selection,
    )
}

pub fn run_client_with_traces_negotiated_transport(
    transport: Transport,
    config: ApalacheConfig,
    trace_paths: Vec<String>,
    selection: &mut CompiledAdapterSelection<'_>,
) -> Result<(), Error> {
    let registration = ClientMessage::RegisterTraces {
        apalache_config: config.clone(),
        itf_trace_paths: trace_paths,
    };
    run_negotiated(transport, config, registration, selection)
}

pub fn run_client_negotiated(
    bin_path: &str,
    config: ApalacheConfig,
    trace_config: TraceGenerationConfig,
    selection: &mut CompiledAdapterSelection<'_>,
    spec: Option<ApalacheSpec>,
) -> Result<(), Error> {
    run_client_negotiated_transport(
        spawn_mirror(bin_path)?,
        config,
        trace_config,
        selection,
        spec,
    )
}

pub fn run_client_negotiated_transport(
    transport: Transport,
    config: ApalacheConfig,
    trace_config: TraceGenerationConfig,
    selection: &mut CompiledAdapterSelection<'_>,
    spec: Option<ApalacheSpec>,
) -> Result<(), Error> {
    let registration = ClientMessage::Register {
        apalache_config: config.clone(),
        trace_config,
        spec,
    };
    run_negotiated(transport, config, registration, selection)
}
