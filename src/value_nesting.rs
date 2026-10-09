//! Nesting checks for attribute values outside an item or an expression: keys,
//! `ExclusiveStartKey`, the legacy `Expected`, `AttributeUpdates` and filter
//! conditions, and PartiQL parameters.
//!
//! DynamoDB caps a value at 32 levels wherever it appears, and refuses one
//! nested that deep before looking up the table or matching a key against its
//! schema. Where the message goes depends on the operation (captured in
//! us-east-1 and eu-west-2 on 2026-10-09 at 31 to 62 levels, with eight more
//! regions sampled where those two disagreed):
//!
//! - bare on GetItem, BatchGetItem, BatchWriteItem's DeleteRequest, a Query's
//!   `ExclusiveStartKey`, `QueryFilter` and `KeyConditions`, and the keys of
//!   TransactGetItems and a transaction's Delete, ConditionCheck and Update
//!   (the transactional ones at the top level, not as a cancellation);
//! - inside the validation envelope on DeleteItem's and UpdateItem's `Key`,
//!   the `Expected` of PutItem, DeleteItem and UpdateItem, and UpdateItem's
//!   `AttributeUpdates`;
//! - after the starting-key prefix for a Scan's `ExclusiveStartKey`;
//! - bare for a `ScanFilter` value from 33 levels.
//!
//! The last two follow seven of the ten regions sampled. eu-west-2,
//! eu-central-1 and ap-southeast-2 answer a Scan's starting key bare below 63
//! levels and refuse a `ScanFilter` value from 32.
//!
//! A PartiQL parameter, like a `ScanFilter` value, gets one more level:
//! DynamoDB runs a statement whose parameter is nested 32 levels and refuses
//! one nested 33.
//!
//! The checks stop looking one level past the limit, so a value built deeper
//! in-process can't run them out of stack.

use std::collections::HashMap;

use crate::errors::{DynoxideError, Result};
use crate::types::AttributeValue;
use crate::validation::{NESTING_LIMIT_MESSAGE, envelope_message};

/// A leaf wrapped in this many maps or lists is refused.
const LIMIT: usize = 32;

/// Whether `value` wraps more than `limit` maps or lists around any leaf.
fn nests_deeper_than(value: &AttributeValue, limit: usize) -> bool {
    match value {
        AttributeValue::M(map) => {
            limit == 0 || map.values().any(|v| nests_deeper_than(v, limit - 1))
        }
        AttributeValue::L(list) => {
            limit == 0 || list.iter().any(|v| nests_deeper_than(v, limit - 1))
        }
        _ => false,
    }
}

/// As [`nests_deeper_than`], for an attribute value still in its JSON form.
fn json_nests_deeper_than(value: &serde_json::Value, limit: usize) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if let Some(map) = object.get("M").and_then(|m| m.as_object()) {
        return limit == 0 || map.values().any(|v| json_nests_deeper_than(v, limit - 1));
    }
    if let Some(list) = object.get("L").and_then(|l| l.as_array()) {
        return limit == 0 || list.iter().any(|v| json_nests_deeper_than(v, limit - 1));
    }
    false
}

fn too_deep(value: &AttributeValue) -> bool {
    nests_deeper_than(value, LIMIT - 1)
}

fn bare() -> DynoxideError {
    DynoxideError::ValidationException(NESTING_LIMIT_MESSAGE.to_string())
}

fn enveloped() -> DynoxideError {
    DynoxideError::ValidationException(envelope_message(NESTING_LIMIT_MESSAGE))
}

/// A key on GetItem, BatchGetItem, BatchWriteItem, TransactGetItems or a
/// transaction's Delete, ConditionCheck or Update: refused bare.
pub(crate) fn check_key(key: &HashMap<String, AttributeValue>) -> Result<()> {
    if key.values().any(too_deep) {
        return Err(bare());
    }
    Ok(())
}

/// A key on DeleteItem or UpdateItem: refused inside the envelope.
pub(crate) fn check_key_enveloped(key: &HashMap<String, AttributeValue>) -> Result<()> {
    if key.values().any(too_deep) {
        return Err(enveloped());
    }
    Ok(())
}

/// A Scan's `ExclusiveStartKey`: refused after the starting-key prefix.
pub(crate) fn check_scan_start_key(key: Option<&HashMap<String, AttributeValue>>) -> Result<()> {
    if key.is_some_and(|k| k.values().any(too_deep)) {
        return Err(DynoxideError::ValidationException(format!(
            "The provided starting key is invalid: {NESTING_LIMIT_MESSAGE}"
        )));
    }
    Ok(())
}

/// A Query's `ExclusiveStartKey`: refused bare.
pub(crate) fn check_query_start_key(key: Option<&HashMap<String, AttributeValue>>) -> Result<()> {
    if key.is_some_and(|k| k.values().any(too_deep)) {
        return Err(bare());
    }
    Ok(())
}

/// The `Value` of each `AttributeUpdates` entry: refused inside the envelope.
pub(crate) fn check_attribute_updates<'a>(
    values: impl IntoIterator<Item = Option<&'a AttributeValue>>,
) -> Result<()> {
    if values.into_iter().flatten().any(too_deep) {
        return Err(enveloped());
    }
    Ok(())
}

/// Whether any `Value` or `AttributeValueList` entry in a legacy condition map
/// (`Expected`, `ScanFilter`) nests too deep. Malformed entries are left to the
/// checks that already report them.
fn conditions_nest_too_deep(conditions: Option<&serde_json::Value>, limit: usize) -> bool {
    let Some(conditions) = conditions.and_then(|c| c.as_object()) else {
        return false;
    };
    conditions.values().filter_map(|c| c.as_object()).any(|c| {
        c.get("Value")
            .is_some_and(|v| json_nests_deeper_than(v, limit))
            || c.get("AttributeValueList")
                .and_then(|l| l.as_array())
                .is_some_and(|l| l.iter().any(|v| json_nests_deeper_than(v, limit)))
    })
}

/// The `Expected` of PutItem, DeleteItem or UpdateItem: refused inside the
/// envelope.
pub(crate) fn check_expected(expected: Option<&serde_json::Value>) -> Result<()> {
    if conditions_nest_too_deep(expected, LIMIT - 1) {
        return Err(enveloped());
    }
    Ok(())
}

/// A `ScanFilter`: refused bare from 33 levels.
pub(crate) fn check_scan_filter(filter: Option<&serde_json::Value>) -> Result<()> {
    if conditions_nest_too_deep(filter, LIMIT) {
        return Err(bare());
    }
    Ok(())
}

/// A Query's `QueryFilter` or `KeyConditions`: refused bare.
pub(crate) fn check_query_conditions(conditions: Option<&serde_json::Value>) -> Result<()> {
    if conditions_nest_too_deep(conditions, LIMIT - 1) {
        return Err(bare());
    }
    Ok(())
}

/// Whether any PartiQL parameter nests 33 or more levels.
///
/// DynamoDB refuses such a parameter before its statement is parsed or its
/// table looked up, with the full nesting message: at the top level on
/// ExecuteStatement and ExecuteTransaction (a transaction is not cancelled),
/// and against that statement alone in a BatchExecuteStatement. A parameter
/// nested 32 levels still runs.
pub(crate) fn parameters_nest_too_deep(parameters: &[AttributeValue]) -> bool {
    parameters.iter().any(|p| nests_deeper_than(p, LIMIT))
}

/// The error for a PartiQL parameter that [`parameters_nest_too_deep`] refuses.
pub(crate) fn deep_parameter_error() -> DynoxideError {
    bare()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deep_map(depth: usize) -> AttributeValue {
        let mut v = AttributeValue::S("leaf".into());
        for _ in 0..depth {
            v = AttributeValue::M(HashMap::from([("n".to_string(), v)]));
        }
        v
    }

    fn deep_json(depth: usize) -> serde_json::Value {
        serde_json::to_value(deep_map(depth)).unwrap()
    }

    #[test]
    fn the_limit_is_32_levels_and_one_more_for_a_parameter() {
        assert!(!too_deep(&deep_map(31)));
        assert!(too_deep(&deep_map(32)));
        assert!(!parameters_nest_too_deep(&[deep_map(32)]));
        assert!(parameters_nest_too_deep(&[deep_map(33)]));
        assert!(!json_nests_deeper_than(&deep_json(31), LIMIT - 1));
        assert!(json_nests_deeper_than(&deep_json(32), LIMIT - 1));
    }

    #[test]
    fn a_far_deeper_value_built_in_process_is_refused_without_walking_all_of_it() {
        // Deep enough that walking every level recursively would be a risk.
        let mut v = AttributeValue::S("leaf".into());
        for _ in 0..100_000 {
            v = AttributeValue::L(vec![v]);
        }
        assert!(too_deep(&v));
        // Drop it iteratively: the derived drop would recurse 100,000 levels.
        let mut stack = vec![v];
        while let Some(value) = stack.pop() {
            if let AttributeValue::L(list) = value {
                stack.extend(list);
            }
        }
    }

    #[test]
    fn legacy_conditions_are_read_through_value_and_the_value_list() {
        let expected = serde_json::json!({
            "a": {"Value": deep_json(32)},
        });
        assert!(conditions_nest_too_deep(Some(&expected), LIMIT - 1));
        let filter = serde_json::json!({
            "a": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "x"}, deep_json(32)]},
        });
        assert!(conditions_nest_too_deep(Some(&filter), LIMIT - 1));
        let shallow = serde_json::json!({
            "a": {"Value": deep_json(31), "AttributeValueList": [deep_json(31)]},
            "b": "not an object",
        });
        assert!(!conditions_nest_too_deep(Some(&shallow), LIMIT - 1));
        assert!(!conditions_nest_too_deep(None, LIMIT - 1));
    }
}
