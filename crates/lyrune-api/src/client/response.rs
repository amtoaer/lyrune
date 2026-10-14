//! QQ 响应的字段兼容与历史层级查找规则。
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

pub(super) fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(value_to_string)
}

pub(super) fn integer_field(value: &Value, keys: &[&str]) -> Option<u64> {
    let object = value.as_object()?;
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(integer_value)
}

pub(super) fn integer_array_field(value: &Value, keys: &[&str], index: usize) -> Option<u64> {
    let object = value.as_object()?;
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(Value::as_array)
        .and_then(|values| values.get(index))
        .and_then(integer_value)
}

pub(super) fn integer_value(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

pub(super) fn bool_field(value: &Value, keys: &[&str]) -> Option<bool> {
    let object = value.as_object()?;
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(bool_value)
}

pub(super) fn bool_value(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::Number(value) => value.as_u64().map(|value| value != 0),
        Value::String(value) => match value.as_str() {
            "1" | "true" | "TRUE" => Some(true),
            "0" | "false" | "FALSE" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

pub(super) fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

pub(super) fn find_array_recursively<'a>(
    value: &'a Value,
    keys: &[&str],
) -> Option<&'a Vec<Value>> {
    match value {
        Value::Object(object) => object
            .iter()
            .find_map(|(key, value)| {
                keys.iter()
                    .any(|candidate| key.eq_ignore_ascii_case(candidate))
                    .then(|| value.as_array())
                    .flatten()
            })
            .or_else(|| {
                object
                    .values()
                    .find_map(|value| find_array_recursively(value, keys))
            }),
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_array_recursively(value, keys)),
        _ => None,
    }
}

pub(super) fn find_object_recursively<'a>(
    value: &'a Value,
    keys: &[&str],
) -> Option<&'a Map<String, Value>> {
    match value {
        Value::Object(object) => object
            .iter()
            .find_map(|(key, value)| {
                keys.iter()
                    .any(|candidate| key.eq_ignore_ascii_case(candidate))
                    .then(|| value.as_object())
                    .flatten()
            })
            .or_else(|| {
                object
                    .values()
                    .find_map(|value| find_object_recursively(value, keys))
            }),
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_object_recursively(value, keys)),
        _ => None,
    }
}

pub(super) fn find_string_recursively(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(object) => find_string_in_object(object, keys).or_else(|| {
            object
                .values()
                .find_map(|value| find_string_recursively(value, keys))
        }),
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_string_recursively(value, keys)),
        _ => None,
    }
}

pub(super) fn find_string_in_object(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    object.iter().find_map(|(key, value)| {
        keys.iter()
            .any(|candidate| key.eq_ignore_ascii_case(candidate))
            .then(|| value_to_string(value))
            .flatten()
    })
}

// 这些转换沿用字段读取规则：非法类型视为缺失，数字/字符串保持原有兼容范围。
pub(super) fn optional_string<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(value_to_string(&value))
}

pub(super) fn optional_integer<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(integer_value(&value))
}
