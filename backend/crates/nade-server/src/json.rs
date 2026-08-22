//! Small readers for `serde_json` values that arrive from outside.
//!
//! A spec compiled by a model, a `jsonb` column, a request body: all three are
//! `Value` trees this crate cannot assume the shape of, and all three are read
//! with the same defensive idiom — "the array of strings under this key, or
//! nothing". That idiom was written out five times, in two modules and in three
//! slightly different spellings, before it lived here.

use serde_json::Value;

/// The strings in a JSON array, skipping anything that is not one.
///
/// A missing key, a `null`, and a value that is not an array all give an empty
/// vector — never an error. Callers are reading data a model or a client wrote,
/// where "absent" and "malformed" lead to the same place: the default.
///
/// Borrows. A caller that needs owned strings maps `str::to_owned` over the
/// result, which is one line and makes the allocation visible where it happens.
pub(crate) fn str_array(value: Option<&Value>) -> Vec<&str> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn it_reads_the_strings_and_skips_everything_else() {
        let value = json!({"tools": ["a", 1, null, "b", {"c": 1}, []]});
        assert_eq!(str_array(value.get("tools")), vec!["a", "b"]);
    }

    #[test]
    fn absent_null_and_not_an_array_are_all_empty() {
        let value = json!({"tools": null, "scalar": "a", "object": {}});
        assert!(str_array(value.get("missing")).is_empty());
        assert!(str_array(value.get("tools")).is_empty());
        assert!(str_array(value.get("scalar")).is_empty());
        assert!(str_array(value.get("object")).is_empty());
        assert!(str_array(None).is_empty());
    }

    #[test]
    fn an_empty_array_is_empty_and_unicode_survives() {
        assert!(str_array(Some(&json!([]))).is_empty());
        assert_eq!(
            str_array(Some(&json!(["日本語", "🙂"]))),
            vec!["日本語", "🙂"]
        );
    }
}
