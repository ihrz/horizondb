use serde_json::{Map, Value};

use crate::error::AppError;

pub const DEFAULT_TABLE: &str = "json";
const MAX_KEY_DEPTH: usize = 32;
pub const MAX_KEY_BYTES: usize = 255;
pub const MAX_TABLE_BYTES: usize = 63;

/// Restricts table names to `[A-Za-z_][A-Za-z0-9_]{0,62}` so they can be
/// interpolated into DDL safely.
pub fn validate_table(table: &str) -> Result<(), AppError> {
    let valid = !table.is_empty()
        && table.len() <= MAX_TABLE_BYTES
        && table.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if valid {
        Ok(())
    } else {
        Err(AppError::validation(
            "table name must match [A-Za-z_][A-Za-z0-9_]{0,62}",
        ))
    }
}

pub fn parse_key(key: &str) -> Result<(String, Vec<String>), AppError> {
    let segments: Vec<&str> = key.split('.').collect();
    if segments.iter().any(|segment| segment.is_empty()) {
        return Err(AppError::validation("malformed key"));
    }
    if segments.len() > MAX_KEY_DEPTH {
        return Err(AppError::validation("key nesting too deep"));
    }
    if key.len() > MAX_KEY_BYTES {
        return Err(AppError::validation("key too long"));
    }
    let base = segments[0].to_owned();
    let path = segments[1..]
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    Ok((base, path))
}

pub fn get_nested<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    let mut current = value;
    for segment in path {
        current = current.get(segment)?;
    }
    Some(current)
}

fn object_mut(value: &mut Value) -> Result<&mut Map<String, Value>, AppError> {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    match value.as_object_mut() {
        Some(map) => Ok(map),
        None => Err(AppError::internal("object coercion failed")),
    }
}

pub fn set_nested(root: &mut Value, path: &[String], new_value: Value) -> Result<(), AppError> {
    let Some((last, parents)) = path.split_last() else {
        *root = new_value;
        return Ok(());
    };
    let mut current = root;
    for segment in parents {
        current = object_mut(current)?
            .entry(segment.clone())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    match current.as_object_mut() {
        Some(map) => {
            map.insert(last.clone(), new_value);
            Ok(())
        }
        None => Err(AppError::validation(
            "cannot set nested value on non-object",
        )),
    }
}

pub fn delete_nested(root: &mut Value, path: &[String]) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    let mut current = root;
    for segment in parents {
        match current.get_mut(segment) {
            Some(child) => current = child,
            None => return false,
        }
    }
    current
        .as_object_mut()
        .is_some_and(|map| map.remove(last).is_some())
}

pub fn add_amount(root: &mut Value, path: &[String], amount: f64) -> Result<(), AppError> {
    let current = if path.is_empty() {
        Some(root.clone())
    } else {
        get_nested(root, path).cloned()
    };
    let updated = match current {
        None => amount,
        Some(Value::Number(number)) => number.as_f64().unwrap_or_default() + amount,
        Some(_) => return Err(AppError::validation("cannot add to non-numeric value")),
    };
    set_nested(root, path, Value::from(updated))
}

fn existing_array(root: &Value, path: &[String]) -> Result<Vec<Value>, AppError> {
    match get_nested(root, path).cloned() {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => Ok(items),
        Some(_) => Err(AppError::validation("target value is not an array")),
    }
}

pub fn push_element(root: &mut Value, path: &[String], element: Value) -> Result<(), AppError> {
    let mut items = existing_array(root, path)?;
    items.push(element);
    set_nested(root, path, Value::Array(items))
}

pub fn pull_element(root: &mut Value, path: &[String], element: &Value) -> Result<(), AppError> {
    let items = existing_array(root, path)?;
    let remaining: Vec<Value> = items.into_iter().filter(|item| item != element).collect();
    set_nested(root, path, Value::Array(remaining))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validate_table_accepts_identifiers_only() {
        assert!(validate_table("json").is_ok());
        assert!(validate_table("_users").is_ok());
        assert!(validate_table("Table_9").is_ok());
        assert_eq!(MAX_TABLE_BYTES, 63);
        for hostile in [
            "",
            "1abc",
            "with space",
            "with-dash",
            "quote\"",
            "semi;colon",
            "drop; select",
        ] {
            assert!(validate_table(hostile).is_err(), "{hostile:?}");
        }
        assert!(validate_table(&"x".repeat(MAX_TABLE_BYTES + 1)).is_err());
    }

    #[test]
    fn parse_key_splits_and_validates() {
        assert_eq!(
            parse_key("user.name").unwrap(),
            ("user".to_owned(), vec!["name".to_owned()])
        );
        assert!(parse_key("").is_err());
        assert!(parse_key(".a").is_err());
        assert!(parse_key("a.").is_err());
        assert!(parse_key("a..b").is_err());

        let deep = vec!["a"; MAX_KEY_DEPTH + 1].join(".");
        assert!(parse_key(&deep).is_err());

        let long = "k".repeat(MAX_KEY_BYTES + 1);
        assert!(parse_key(&long).is_err());
    }

    #[test]
    fn get_nested_walks_paths_and_reports_missing() {
        let root = json!({"a": {"b": [1, 2]}});
        assert_eq!(
            get_nested(&root, &["a".to_owned(), "b".to_owned()]),
            Some(&json!([1, 2]))
        );
        assert_eq!(get_nested(&root, &["a".to_owned(), "zzz".to_owned()]), None);
        assert_eq!(get_nested(&root, &[]), Some(&root));
    }

    #[test]
    fn set_nested_creates_intermediates_and_rejects_scalar_leaf_parent() {
        let mut root = json!({});
        set_nested(&mut root, &["a".to_owned(), "b".to_owned()], json!(1)).unwrap();
        assert_eq!(root, json!({"a": {"b": 1}}));

        let mut deep_scalar = json!({"a": 5});
        set_nested(
            &mut deep_scalar,
            &["a".to_owned(), "b".to_owned(), "c".to_owned()],
            json!(1),
        )
        .unwrap();
        assert_eq!(deep_scalar, json!({"a": {"b": {"c": 1}}}));

        let mut leaf_scalar = json!({"a": {"b": 5}});
        assert!(
            set_nested(
                &mut leaf_scalar,
                &["a".to_owned(), "b".to_owned()],
                json!(9)
            )
            .is_ok()
        );

        let mut direct_scalar_leaf = json!({"a": 5});
        assert!(
            set_nested(
                &mut direct_scalar_leaf,
                &["a".to_owned(), "b".to_owned()],
                json!(9)
            )
            .is_err()
        );

        let mut scalar = json!(42);
        assert!(set_nested(&mut scalar, &["x".to_owned()], json!(1)).is_err());
    }

    #[test]
    fn set_nested_with_empty_path_replaces_root() {
        let mut root = json!({"old": true});
        set_nested(&mut root, &[], json!([1])).unwrap();
        assert_eq!(root, json!([1]));
    }

    #[test]
    fn delete_nested_removes_leaf_and_reports_missing() {
        let mut root = json!({"a": {"b": 1}});
        assert!(delete_nested(&mut root, &["a".to_owned(), "b".to_owned()]));
        assert_eq!(root, json!({"a": {}}));
        assert!(!delete_nested(
            &mut root,
            &["a".to_owned(), "zzz".to_owned()]
        ));
        assert!(!delete_nested(&mut root, &[]));
    }

    #[test]
    fn add_amount_creates_updates_and_rejects_non_numeric() {
        let mut root = json!({});
        add_amount(&mut root, &["counter".to_owned()], 5.0).unwrap();
        assert_eq!(root, json!({"counter": 5.0}));

        add_amount(&mut root, &["counter".to_owned()], -2.0).unwrap();
        assert_eq!(root, json!({"counter": 3.0}));

        let mut number = json!(10);
        add_amount(&mut number, &[], 5.0).unwrap();
        assert_eq!(number, json!(15.0));

        let mut text = json!("hello");
        assert!(add_amount(&mut text, &[], 1.0).is_err());
        assert!(add_amount(&mut text, &["deep".to_owned()], 1.0).is_err());
    }

    #[test]
    fn push_and_pull_manage_arrays() {
        let mut root = json!({});
        push_element(&mut root, &["list".to_owned()], json!(1)).unwrap();
        push_element(&mut root, &["list".to_owned()], json!(2)).unwrap();
        push_element(&mut root, &["list".to_owned()], json!(1)).unwrap();
        assert_eq!(root, json!({"list": [1, 2, 1]}));

        pull_element(&mut root, &["list".to_owned()], &json!(1)).unwrap();
        assert_eq!(root, json!({"list": [2]}));

        let mut scalar = json!(3);
        assert!(push_element(&mut scalar, &["list".to_owned()], json!(1)).is_err());
        assert!(pull_element(&mut scalar, &["list".to_owned()], &json!(1)).is_err());
    }

    #[test]
    fn table_names_are_restricted_to_identifier_charset() {
        for name in ["json", "_private", "Table9", "a"] {
            assert!(validate_table(name).is_ok(), "{name} should be valid");
        }

        let long_name = "a".repeat(MAX_TABLE_BYTES + 1);
        for name in [
            "",
            "9start",
            "-dash",
            "with space",
            "with.dot",
            long_name.as_str(),
        ] {
            assert!(validate_table(name).is_err(), "{name} should be rejected");
        }
    }
}
