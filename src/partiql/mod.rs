//! PartiQL statement parsing and execution for DynamoDB.
//!
//! Supports SELECT, INSERT, UPDATE, DELETE statements mapped to
//! underlying DynamoDB operations.

pub mod executor;
pub mod parser;

/// One step of a document path: a map key or a list index.
enum PathSegment<'a> {
    Key(&'a str),
    Index(usize),
}

/// Split a path like `"a.b[0].c"` into segments.
/// Returns None if the path contains malformed bracket expressions (e.g. `a[xyz]`).
fn split_path_segments(path: &str) -> Option<Vec<PathSegment<'_>>> {
    let mut segments = Vec::new();
    let bytes = path.as_bytes();
    let mut start = 0;
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'.' => {
                if start < i {
                    segments.push(PathSegment::Key(&path[start..i]));
                }
                i += 1;
                start = i;
            }
            b'[' => {
                if start < i {
                    segments.push(PathSegment::Key(&path[start..i]));
                }
                i += 1;
                let idx_start = i;
                while i < bytes.len() && bytes[i] != b']' {
                    i += 1;
                }
                let idx = path[idx_start..i].parse::<usize>().ok()?;
                segments.push(PathSegment::Index(idx));
                if i < bytes.len() {
                    i += 1; // skip ']'
                }
                start = i;
                // Skip a trailing dot after ']' (e.g. `a[0].b`)
                if i < bytes.len() && bytes[i] == b'.' {
                    i += 1;
                    start = i;
                }
            }
            _ => {
                i += 1;
            }
        }
    }

    if start < bytes.len() {
        segments.push(PathSegment::Key(&path[start..]));
    }

    Some(segments)
}

/// The longest statement DynamoDB takes, in code points.
const MAX_STATEMENT_LENGTH: usize = 8192;

/// DynamoDB's member constraint on a statement's length, at `path`: an empty
/// statement or one over 8,192 code points (not bytes or UTF-16 units) is
/// refused as a request member before it is parsed, echoing the statement.
/// The violation's text, for [`member_rejection`] to list with the request's
/// others. Captured in eu-west-2 and us-east-1, 2026-10-10.
pub(crate) fn statement_length_violation(statement: &str, path: &str) -> Option<String> {
    let constraint = match statement.chars().count() {
        0 => "Member must have length greater than or equal to 1",
        n if n > MAX_STATEMENT_LENGTH => "Member must have length less than or equal to 8192",
        _ => return None,
    };
    Some(format!(
        "Value '{statement}' at '{path}' failed to satisfy constraint: {constraint}"
    ))
}

/// DynamoDB's member constraint on a statement's `Parameters`, at `path`: a
/// list given with nothing in it is refused as a request member. Captured in
/// eu-west-2 and us-east-1, 2026-10-10, on ExecuteStatement and on batch and
/// transaction members.
pub(crate) fn parameters_violation<T>(parameters: Option<&[T]>, path: &str) -> Option<String> {
    parameters.filter(|p| p.is_empty()).map(|_| {
        format!(
            "Value '[]' at '{path}' failed to satisfy constraint: Member must have length \
             greater than or equal to 1"
        )
    })
}

/// A request's member violations as the one error DynamoDB returns for them:
/// every one listed, numbered and joined in member order (captured eu-west-2
/// and us-east-1, 2026-10-10), or `None` when there are none.
pub(crate) fn member_rejection(violations: &[String]) -> Option<crate::errors::DynoxideError> {
    crate::validation::format_validation_errors(violations)
        .map(crate::errors::DynoxideError::ValidationException)
}
