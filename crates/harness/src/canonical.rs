//! A JSON canonicalization used everywhere two independently-produced JSON
//! values need to compare or hash equal regardless of key order or exact
//! numeric representation — the two things a JSON round trip is free to change
//! without changing meaning (`abstract.md` H5).

use serde_json::Value;

/// Recursively sorts object keys and coerces every number to its `f64` text
/// form, then serializes. `1` and `1.0` compare equal; an object's own key
/// order stops mattering. Precision above 2^53 is still lost — canonicalizing
/// doesn't recover what a lossy round trip already discarded, it just stops that loss from
/// registering as a content *change* on a faithful round trip.
pub fn canonical_json(value: &Value) -> String {
    normalize(value).to_string()
}

fn normalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .iter()
                .map(|(key, value)| (key.clone(), normalize(value)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(normalize).collect()),
        Value::Number(number) => match number.as_f64() {
            Some(float) => serde_json::json!(float),
            None => value.clone(),
        },
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_order_does_not_matter() {
        let a = serde_json::json!({"a": 1, "b": 2});
        let b = serde_json::json!({"b": 2, "a": 1});
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn an_integer_and_its_float_form_compare_equal() {
        let a = serde_json::json!({"x": 1});
        let b = serde_json::json!({"x": 1.0});
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn a_real_difference_still_differs() {
        let a = serde_json::json!({"x": 1});
        let b = serde_json::json!({"x": 2});
        assert_ne!(canonical_json(&a), canonical_json(&b));
    }
}
