//! The JSON Schema checks the core runs: every tool call against its tool's
//! `input_schema`, and a manifest's default config against its config schema.
//!
//! A subset, written here rather than pulled in: `type`, `enum`, `const`,
//! `properties`, `required`, `additionalProperties`, `items`, `minItems`,
//! `maxItems`, `minLength`, `maxLength`, `minimum`, `maximum`, `allOf`,
//! `anyOf` and `oneOf`. Keywords outside it are ignored, never refused, so a
//! schema written for a full validator is checked as far as this one reaches
//! and the plugin still sees only input that passed that far.

use serde_json::Value;

/// One failed check, at a JSON Pointer into the instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

/// Every violation of `schema` in `instance`; empty when it passes.
pub fn check(schema: &Value, instance: &Value) -> Vec<Violation> {
    let mut found = Vec::new();
    walk(schema, instance, "", &mut found);
    found
}

/// Whether a value is a schema at all: an object or a boolean.
pub fn is_schema(value: &Value) -> bool {
    matches!(value, Value::Object(_) | Value::Bool(_))
}

fn walk(schema: &Value, instance: &Value, path: &str, found: &mut Vec<Violation>) {
    let schema = match schema {
        Value::Bool(true) => return,
        Value::Bool(false) => {
            found.push(violation(path, "no value is allowed here"));
            return;
        }
        Value::Object(schema) => schema,
        _ => return,
    };

    if let Some(expected) = schema.get("type")
        && !type_matches(expected, instance)
    {
        found.push(violation(
            path,
            format!(
                "expected {}, got {}",
                describe_type(expected),
                kind_of(instance)
            ),
        ));
        // Every keyword below assumes the type; checking them against the
        // wrong one only restates this violation less clearly.
        return;
    }

    if let Some(Value::Array(options)) = schema.get("enum")
        && !options.contains(instance)
    {
        found.push(violation(path, format!("must be one of {}", list(options))));
    }
    if let Some(constant) = schema.get("const")
        && constant != instance
    {
        found.push(violation(path, format!("must be {constant}")));
    }

    match instance {
        Value::Object(object) => {
            if let Some(Value::Array(required)) = schema.get("required") {
                for key in required.iter().filter_map(Value::as_str) {
                    if !object.contains_key(key) {
                        found.push(violation(path, format!("`{key}` is required")));
                    }
                }
            }
            let properties = schema.get("properties").and_then(Value::as_object);
            for (key, value) in object {
                let child = format!("{path}/{}", escape(key));
                match properties.and_then(|properties| properties.get(key)) {
                    Some(property) => walk(property, value, &child, found),
                    None => match schema.get("additionalProperties") {
                        Some(Value::Bool(false)) => {
                            found.push(violation(path, format!("`{key}` is not allowed")));
                        }
                        Some(additional) => walk(additional, value, &child, found),
                        None => {}
                    },
                }
            }
        }
        Value::Array(items) => {
            if let Some(min) = schema.get("minItems").and_then(Value::as_u64)
                && (items.len() as u64) < min
            {
                found.push(violation(path, format!("needs at least {min} items")));
            }
            if let Some(max) = schema.get("maxItems").and_then(Value::as_u64)
                && (items.len() as u64) > max
            {
                found.push(violation(path, format!("allows at most {max} items")));
            }
            if let Some(item) = schema.get("items") {
                for (index, value) in items.iter().enumerate() {
                    walk(item, value, &format!("{path}/{index}"), found);
                }
            }
        }
        Value::String(text) => {
            let length = text.chars().count() as u64;
            if let Some(min) = schema.get("minLength").and_then(Value::as_u64)
                && length < min
            {
                found.push(violation(path, format!("needs at least {min} characters")));
            }
            if let Some(max) = schema.get("maxLength").and_then(Value::as_u64)
                && length > max
            {
                found.push(violation(path, format!("allows at most {max} characters")));
            }
        }
        Value::Number(number) => {
            let number = number.as_f64().unwrap_or(f64::NAN);
            if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
                && number < min
            {
                found.push(violation(path, format!("must be at least {min}")));
            }
            if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
                && number > max
            {
                found.push(violation(path, format!("must be at most {max}")));
            }
        }
        _ => {}
    }

    if let Some(Value::Array(all)) = schema.get("allOf") {
        for each in all {
            walk(each, instance, path, found);
        }
    }
    if let Some(Value::Array(any)) = schema.get("anyOf")
        && !any
            .iter()
            .any(|each| check_at(each, instance, path).is_empty())
    {
        found.push(violation(path, "matches none of the allowed shapes"));
    }
    if let Some(Value::Array(one)) = schema.get("oneOf") {
        let matching = one
            .iter()
            .filter(|each| check_at(each, instance, path).is_empty())
            .count();
        if matching != 1 {
            found.push(violation(
                path,
                format!("must match exactly one allowed shape; matches {matching}"),
            ));
        }
    }
}

fn check_at(schema: &Value, instance: &Value, path: &str) -> Vec<Violation> {
    let mut found = Vec::new();
    walk(schema, instance, path, &mut found);
    found
}

fn type_matches(expected: &Value, instance: &Value) -> bool {
    match expected {
        Value::String(name) => is_type(name, instance),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .any(|name| is_type(name, instance)),
        _ => true,
    }
}

fn is_type(name: &str, instance: &Value) -> bool {
    match name {
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "string" => instance.is_string(),
        "boolean" => instance.is_boolean(),
        "null" => instance.is_null(),
        "number" => instance.is_number(),
        "integer" => match instance {
            Value::Number(number) => {
                number.is_i64()
                    || number.is_u64()
                    || number.as_f64().is_some_and(|value| value.fract() == 0.0)
            }
            _ => false,
        },
        // An unknown type name is the schema's problem, not the instance's.
        _ => true,
    }
}

fn describe_type(expected: &Value) -> String {
    match expected {
        Value::String(name) => name.clone(),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" or "),
        other => other.to_string(),
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn list(options: &[Value]) -> String {
    options
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn violation(path: &str, message: impl Into<String>) -> Violation {
    Violation {
        path: path.to_owned(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "minLength": 1 },
                "count": { "type": "integer", "minimum": 0 },
                "mode": { "enum": ["a", "b"] },
                "tags": { "type": "array", "items": { "type": "string" } },
            },
            "required": ["name"],
            "additionalProperties": false,
        })
    }

    #[test]
    fn a_valid_instance_passes() {
        assert!(
            check(
                &schema(),
                &json!({ "name": "x", "count": 2, "tags": ["a"] })
            )
            .is_empty()
        );
    }

    #[test]
    fn each_violation_names_where_and_what() {
        let found = check(
            &schema(),
            &json!({ "count": -1, "mode": "c", "tags": [1], "extra": true }),
        );
        let rendered: Vec<String> = found.iter().map(ToString::to_string).collect();
        assert!(rendered.contains(&"`name` is required".to_owned()));
        assert!(rendered.contains(&"/count: must be at least 0".to_owned()));
        assert!(rendered.contains(&"/mode: must be one of \"a\", \"b\"".to_owned()));
        assert!(rendered.contains(&"/tags/0: expected string, got number".to_owned()));
        assert!(rendered.contains(&"`extra` is not allowed".to_owned()));
    }

    #[test]
    fn integer_accepts_whole_floats_and_refuses_fractions() {
        let schema = json!({ "type": "integer" });
        assert!(check(&schema, &json!(3.0)).is_empty());
        assert!(!check(&schema, &json!(3.5)).is_empty());
    }

    #[test]
    fn one_of_needs_exactly_one_match() {
        let schema =
            json!({ "oneOf": [{ "type": "string" }, { "type": "string", "minLength": 2 }] });
        assert!(check(&schema, &json!("a")).is_empty());
        assert!(!check(&schema, &json!("ab")).is_empty());
    }
}
