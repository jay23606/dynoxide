//! PartiQL statement parsing and execution for DynamoDB.
//!
//! Supports SELECT, INSERT, UPDATE, DELETE statements mapped to
//! underlying DynamoDB operations.

pub mod executor;
pub mod parser;

/// The longest statement DynamoDB takes, in code points.
const MAX_STATEMENT_LENGTH: usize = 8192;

/// DynamoDB's member constraint on a statement's length, at `path`: an empty
/// statement or one over 8,192 code points (not bytes or UTF-16 units) is
/// refused as a request member before it is parsed, echoing the statement.
/// Captured in eu-west-2 and us-east-1, 2026-10-10.
pub(crate) fn statement_length_rejection(
    statement: &str,
    path: &str,
) -> Option<crate::errors::DynoxideError> {
    let constraint = match statement.chars().count() {
        0 => "Member must have length greater than or equal to 1",
        n if n > MAX_STATEMENT_LENGTH => "Member must have length less than or equal to 8192",
        _ => return None,
    };
    Some(crate::errors::DynoxideError::ValidationException(
        crate::validation::envelope_message(&format!(
            "Value '{statement}' at '{path}' failed to satisfy constraint: {constraint}"
        )),
    ))
}
