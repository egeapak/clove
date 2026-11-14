//! Tool arguments that refuse a key the tool does not define.
//!
//! The shared argument structs (`clove_types::tool`) use `#[serde(flatten)]`,
//! and serde cannot combine `flatten` with `deny_unknown_fields`. So a
//! misspelled or made-up argument used to be dropped without a word:
//! `clove_list {"labels": ["x"]}` (the key is `label`) returned the whole store,
//! and `clove_comments {"offset": 1}` (the key is `skip_newest`) returned the
//! newest page again. [`Checked`] closes that gap after the fact: it compares
//! the keys the client sent with the properties of the tool's published
//! `inputSchema`, so the accepted set can never drift from the advertised one.
//!
//! An unknown key is reported as a tool error (`isError`), like every other
//! bad argument value, and the message lists the valid names so an agent can
//! correct the call in one step.

use std::borrow::Cow;

use rmcp::schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// `T`, plus the argument keys the client sent that `T` does not define.
#[derive(Debug, Clone)]
pub struct Checked<T> {
    args: T,
    unknown: Vec<String>,
    valid: Vec<String>,
}

impl<T> Checked<T> {
    /// The arguments, or the message for a call that named an unknown one.
    pub fn into_args(self) -> Result<T, String> {
        if self.unknown.is_empty() {
            return Ok(self.args);
        }
        let noun = if self.unknown.len() == 1 {
            "argument"
        } else {
            "arguments"
        };
        Err(format!(
            "VALIDATION_ERROR: unknown {noun} `{}`; this tool accepts: {}",
            self.unknown.join("`, `"),
            self.valid.join(", ")
        ))
    }
}

/// The argument names `T` publishes: the `properties` of its JSON Schema.
/// `flatten`ed structs are merged into that one object by the derive.
fn property_names<T: JsonSchema>() -> Vec<String> {
    let schema = SchemaGenerator::default().into_root_schema_for::<T>();
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| props.keys().cloned().collect())
        .unwrap_or_default()
}

impl<'de, T> Deserialize<'de> for Checked<T>
where
    T: DeserializeOwned + JsonSchema,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let valid = property_names::<T>();
        let unknown = value
            .as_object()
            .map(|sent| {
                sent.keys()
                    .filter(|key| !valid.contains(key))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let args = T::deserialize(value).map_err(serde::de::Error::custom)?;
        Ok(Checked {
            args,
            unknown,
            valid,
        })
    }
}

/// The published schema is `T`'s own, closed with `additionalProperties:
/// false` so a client that validates its call sees the same rule the server
/// applies.
impl<T: JsonSchema> JsonSchema for Checked<T> {
    fn schema_name() -> Cow<'static, str> {
        T::schema_name()
    }

    fn schema_id() -> Cow<'static, str> {
        T::schema_id()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let mut schema = T::json_schema(generator);
        schema.insert("additionalProperties".to_owned(), Value::Bool(false));
        schema
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clove_types::tool::{CommentsArgs, EditArgs, ListArgs, SearchArgs, StatsArgs};
    use serde_json::json;

    fn parse<T: DeserializeOwned + JsonSchema>(v: Value) -> Result<T, String> {
        serde_json::from_value::<Checked<T>>(v)
            .expect("well-typed arguments deserialize")
            .into_args()
    }

    #[test]
    fn known_arguments_pass_including_flattened_ones() {
        let a: ListArgs = parse(json!({
            "label": ["a"], "status": "open", "fields": ["id"], "compact": false, "limit": 5
        }))
        .unwrap();
        assert_eq!(a.filter.limit, Some(5));
        assert_eq!(a.filter.shape.fields, Some(vec!["id".to_owned()]));
        parse::<StatsArgs>(json!({})).unwrap();
    }

    #[test]
    fn a_misspelled_filter_is_refused_and_the_valid_names_are_listed() {
        let err = parse::<ListArgs>(json!({ "labels": ["area:ios"] })).unwrap_err();
        assert!(err.contains("unknown argument `labels`"), "{err}");
        assert!(err.contains("label,"), "names the real key: {err}");
    }

    #[test]
    fn offset_is_not_an_argument_of_the_comment_window() {
        let err = parse::<CommentsArgs>(json!({ "id": "p-1", "offset": 1 })).unwrap_err();
        assert!(err.contains("`offset`"), "{err}");
        assert!(err.contains("skip_newest"), "{err}");
    }

    #[test]
    fn several_unknown_arguments_are_all_named() {
        let err = parse::<EditArgs>(json!({ "id": "p-1", "labels": ["x"], "parent": "p-2" }))
            .unwrap_err();
        assert!(err.contains("unknown arguments"), "{err}");
        assert!(err.contains("labels") && err.contains("parent"), "{err}");
        assert!(err.contains("add_labels"), "{err}");
    }

    #[test]
    fn search_takes_no_field_filters() {
        let err = parse::<SearchArgs>(json!({ "text": "x", "status": "closed" })).unwrap_err();
        assert!(err.contains("`status`"), "{err}");
    }

    #[test]
    fn the_published_schema_is_closed() {
        let schema = SchemaGenerator::default().into_root_schema_for::<Checked<ListArgs>>();
        assert_eq!(
            schema.get("additionalProperties"),
            Some(&Value::Bool(false))
        );
        assert!(schema.get("properties").unwrap().get("label").is_some());
    }
}
