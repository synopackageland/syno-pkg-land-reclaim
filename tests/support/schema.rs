//! A validator for the published JSON Schemas (`schemas/v1`, SPEC 14),
//! shared by the golden and live tests.
//!
//! It covers the keyword subset the schemas use and rejects any other
//! keyword. It is stricter than the schemas: undeclared object properties
//! are errors here, so every emitted field must be documented, while
//! consumers stay free to tolerate additive changes.

use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

pub fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

pub fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path:?}: {e}"))
}

pub struct Schemas {
    pub files: BTreeMap<String, Value>,
}

impl Schemas {
    pub fn load() -> Self {
        let dir = root().join("schemas/v1");
        let files = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{dir:?}: {e}"))
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().ends_with(".schema.json"))
            .map(|p| {
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    read_json(&p),
                )
            })
            .collect();
        Schemas { files }
    }

    /// Errors of `value` against the command's schema; empty if valid.
    pub fn validate(&self, command: &str, value: &Value) -> Vec<String> {
        let file = format!("{command}.schema.json");
        self.validate_file(&file, value)
    }

    /// Errors of a value against a named schema file.
    pub fn validate_file(&self, file: &str, value: &Value) -> Vec<String> {
        let schema = &self.files[file];
        let mut errors = Vec::new();
        self.check(file, schema, value, "$", true, &mut errors);
        errors
    }

    /// `file` and the schema a `$ref` names, relative to `base`.
    fn resolve<'a>(&'a self, base: &'a str, reference: &'a str) -> (&'a str, &'a Value) {
        let (file, pointer) = reference.split_once('#').unwrap_or((reference, ""));
        let file = if file.is_empty() { base } else { file };
        let doc = self
            .files
            .get(file)
            .unwrap_or_else(|| panic!("{base}: unknown schema file in {reference}"));
        let target = doc
            .pointer(pointer)
            .unwrap_or_else(|| panic!("{base}: dangling $ref {reference}"));
        (file, target)
    }

    /// Validates `value`; with `strict`, undeclared properties are errors.
    fn check(
        &self,
        base: &str,
        schema: &Value,
        value: &Value,
        at: &str,
        strict: bool,
        errors: &mut Vec<String>,
    ) {
        let schema = schema.as_object().expect("schema is an object");
        for (keyword, arg) in schema {
            match keyword.as_str() {
                "$schema" | "$id" | "title" | "description" | "$defs" => {}
                "$ref" => {
                    let (file, target) = self.resolve(base, arg.as_str().unwrap());
                    self.check(file, target, value, at, strict, errors);
                }
                "allOf" => {
                    for part in arg.as_array().unwrap() {
                        self.check(base, part, value, at, false, errors);
                    }
                }
                "anyOf" => {
                    let fits = arg.as_array().unwrap().iter().any(|part| {
                        let mut e = Vec::new();
                        self.check(base, part, value, at, strict, &mut e);
                        e.is_empty()
                    });
                    if !fits {
                        errors.push(format!("{at}: matches no alternative"));
                    }
                }
                "type" => {
                    let types: Vec<&str> = match arg {
                        Value::String(t) => vec![t],
                        Value::Array(ts) => ts.iter().map(|t| t.as_str().unwrap()).collect(),
                        _ => panic!("bad type {arg}"),
                    };
                    if !types.iter().any(|t| has_type(value, t)) {
                        errors.push(format!("{at}: expected {types:?}, got {value}"));
                    }
                }
                "enum" => {
                    if !arg.as_array().unwrap().contains(value) {
                        errors.push(format!("{at}: {value} not in {arg}"));
                    }
                }
                "const" => {
                    if arg != value {
                        errors.push(format!("{at}: {value} is not {arg}"));
                    }
                }
                "minimum" => {
                    if let Some(n) = value.as_f64()
                        && n < arg.as_f64().unwrap()
                    {
                        errors.push(format!("{at}: {n} below {arg}"));
                    }
                }
                "maximum" => {
                    if let Some(n) = value.as_f64()
                        && n > arg.as_f64().unwrap()
                    {
                        errors.push(format!("{at}: {n} above {arg}"));
                    }
                }
                "pattern" => {
                    if let Some(s) = value.as_str()
                        && !matches_pattern(arg.as_str().unwrap(), s)
                    {
                        errors.push(format!("{at}: {s:?} does not match {arg}"));
                    }
                }
                "required" => {
                    if let Some(object) = value.as_object() {
                        for name in arg.as_array().unwrap() {
                            if !object.contains_key(name.as_str().unwrap()) {
                                errors.push(format!("{at}: missing {name}"));
                            }
                        }
                    }
                }
                "properties" => {
                    if let Some(object) = value.as_object() {
                        for (name, sub) in arg.as_object().unwrap() {
                            if let Some(v) = object.get(name) {
                                self.check(base, sub, v, &format!("{at}.{name}"), true, errors);
                            }
                        }
                    }
                }
                "additionalProperties" => {
                    // The validator already rejects undeclared properties below.
                    assert_eq!(
                        arg,
                        &Value::Bool(false),
                        "{base}: only additionalProperties false is supported"
                    );
                }
                "minItems" | "maxItems" => {
                    if let Some(items) = value.as_array() {
                        let limit = arg.as_u64().unwrap() as usize;
                        if (keyword.as_str() == "minItems" && items.len() < limit)
                            || (keyword.as_str() == "maxItems" && items.len() > limit)
                        {
                            errors
                                .push(format!("{at}: {} violates {keyword} {limit}", items.len()));
                        }
                    }
                }
                "items" => {
                    if let Some(items) = value.as_array() {
                        for (i, v) in items.iter().enumerate() {
                            self.check(base, arg, v, &format!("{at}[{i}]"), true, errors);
                        }
                    }
                }
                other => panic!("{base}: unsupported keyword {other}"),
            }
        }
        // `$ref` targets and `anyOf` alternatives check strictly themselves.
        if strict
            && !schema.contains_key("$ref")
            && !schema.contains_key("anyOf")
            && let Some(object) = value.as_object()
        {
            let mut declared = Vec::new();
            self.declared(base, schema, &mut declared);
            for name in object.keys() {
                if !declared.contains(name) {
                    errors.push(format!("{at}: undeclared property {name}"));
                }
            }
        }
    }

    /// Property names declared by `schema` and the parts it is made of.
    fn declared(&self, base: &str, schema: &Map<String, Value>, out: &mut Vec<String>) {
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            out.extend(properties.keys().cloned());
        }
        for part in schema
            .get("allOf")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let part = part.as_object().unwrap();
            match part.get("$ref") {
                Some(r) => {
                    let (file, target) = self.resolve(base, r.as_str().unwrap());
                    self.declared(file, target.as_object().unwrap(), out);
                }
                None => self.declared(base, part, out),
            }
        }
    }
}

fn has_type(value: &Value, name: &str) -> bool {
    match name {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "integer" => value.is_u64() || value.is_i64(),
        "number" => value.is_number(),
        "string" => value.is_string(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => panic!("unknown type {name}"),
    }
}

/// Published schema patterns: lowercase output hex and exact request path bytes.
fn matches_pattern(pattern: &str, s: &str) -> bool {
    if pattern == "^(?:[0-9a-fA-F]{2})+$" {
        return !s.is_empty() && s.len() % 2 == 0 && s.bytes().all(|byte| byte.is_ascii_hexdigit());
    }
    let dash = match pattern {
        "^[0-9a-f]+$" => false,
        "^[0-9a-f-]+$" => true,
        _ => panic!("unsupported pattern {pattern}"),
    };
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c) || (dash && c == '-'))
}
