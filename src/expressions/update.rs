//! UpdateExpression parsing and evaluation.
//!
//! Supports SET, REMOVE, ADD, DELETE clauses.

use crate::expressions::condition::parse_raw_path;
use crate::expressions::tokenizer::{Token, TokenStream, tokenize, tokenizer_syntax_error};
use crate::expressions::{
    INVALID_UPDATE_PATH, MISSING_ATTRIBUTE, OPERAND_TYPE, PathElement, TrackedExpressionAttributes,
    check_update_parent, format_path_for_error, remove_path, resolve_path, resolve_path_elements,
    set_path,
};
use crate::types::AttributeValue;
use std::collections::HashMap;

/// Parsed update expression with all clause actions.
#[derive(Debug)]
pub struct UpdateExpr {
    pub set_actions: Vec<SetAction>,
    pub remove_actions: Vec<Vec<PathElement>>,
    pub add_actions: Vec<AddAction>,
    pub delete_actions: Vec<DeleteAction>,
}

/// A SET action: `path = value_expr`
#[derive(Debug)]
pub struct SetAction {
    pub path: Vec<PathElement>,
    pub value: SetValue,
}

/// Value expression for SET.
#[derive(Debug)]
pub enum SetValue {
    /// Direct value or path reference
    Operand(SetOperand),
    /// `operand + operand`
    Plus(SetOperand, SetOperand),
    /// `operand - operand`
    Minus(SetOperand, SetOperand),
}

/// An operand in a SET expression.
#[derive(Debug)]
pub enum SetOperand {
    Path(Vec<PathElement>),
    ValueRef(String),
    IfNotExists(Vec<PathElement>, Box<SetOperand>),
    ListAppend(Box<SetOperand>, Box<SetOperand>),
    /// A parenthesised sub-expression, e.g. `(c - :v)`.
    Group(Box<SetValue>),
}

/// An ADD action: `path :value`
#[derive(Debug)]
pub struct AddAction {
    pub path: Vec<PathElement>,
    pub value_ref: String,
}

/// A DELETE action: `path :value`
#[derive(Debug)]
pub struct DeleteAction {
    pub path: Vec<PathElement>,
    pub value_ref: String,
}

impl UpdateExpr {
    /// What DynamoDB charges a standalone `UpdateItem` for this statement when
    /// it holds the request to the 400KB limit.
    ///
    /// The charge comes from the request alone. Nothing the table already
    /// stores enters it: an attribute the statement leaves alone, the rest of a
    /// map or list it writes into, the members a set already holds and the value
    /// a path operand reads are all free. Each clause is charged for its target
    /// path and for the operands it is written with, never for the value it
    /// produces:
    ///
    /// ```text
    /// statement              3 + every clause
    /// SET target = expr      19 + target + expr
    /// ADD target :v          19 + target + size of :v
    /// REMOVE target          1 + target
    /// DELETE target :v       1 + target (the members removed are free)
    ///
    /// target                 first element, plus 4 + element for each later one
    /// path operand           1, plus 14 + element for every element
    /// element                UTF-8 bytes of the resolved name, or the digits
    ///                        of a list index
    ///
    /// :v                     the value's size, at every use
    /// a + b                  34 + a + b
    /// a - b                  37 + a + b
    /// if_not_exists(p, v)    39 + path operand p + v
    /// list_append(a, b)      37 + a + b
    /// ( x )                  x
    /// ```
    ///
    /// Captured against eu-west-2 on 9 October 2026 by bisecting the update
    /// gate across 86 statements, with every accepted item read back and the
    /// next byte refused. The ConditionExpression and ReturnValues add nothing,
    /// and a `#name` placeholder is charged at the length of the name it stands
    /// for. Names and values must already be validated; one that does not
    /// resolve is an error.
    pub fn request_charge(&self, tracker: &TrackedExpressionAttributes) -> Result<usize, String> {
        const STATEMENT: usize = 3;
        const WRITES_A_VALUE: usize = 19;
        const CLEARS: usize = 1;

        let mut charge = STATEMENT;
        for action in &self.set_actions {
            charge += WRITES_A_VALUE
                + target_charge(&action.path, tracker)?
                + set_value_charge(&action.value, tracker)?;
        }
        for action in &self.add_actions {
            charge += WRITES_A_VALUE
                + target_charge(&action.path, tracker)?
                + tracker.resolve_value(&action.value_ref)?.size();
        }
        for path in &self.remove_actions {
            charge += CLEARS + target_charge(path, tracker)?;
        }
        for action in &self.delete_actions {
            charge += CLEARS + target_charge(&action.path, tracker)?;
        }
        Ok(charge)
    }
}

/// The length a path element counts for in the request charge.
fn element_charge(
    element: &PathElement,
    tracker: &TrackedExpressionAttributes,
) -> Result<usize, String> {
    Ok(match element {
        PathElement::Attribute(name) if name.starts_with('#') => tracker.resolve_name(name)?.len(),
        PathElement::Attribute(name) => name.len(),
        PathElement::Index(index) => index.to_string().len(),
    })
}

/// A path the statement writes to: its first element, then four bytes plus
/// the element for each step after it.
fn target_charge(
    path: &[PathElement],
    tracker: &TrackedExpressionAttributes,
) -> Result<usize, String> {
    const STEP: usize = 4;
    let mut charge = 0;
    for (i, element) in path.iter().enumerate() {
        charge += element_charge(element, tracker)? + if i == 0 { 0 } else { STEP };
    }
    Ok(charge)
}

/// A path the statement reads from. Charged for the path alone; the value it
/// reads is free.
fn path_operand_charge(
    path: &[PathElement],
    tracker: &TrackedExpressionAttributes,
) -> Result<usize, String> {
    const OPERAND: usize = 1;
    const STEP: usize = 14;
    let mut charge = OPERAND;
    for element in path {
        charge += STEP + element_charge(element, tracker)?;
    }
    Ok(charge)
}

fn set_value_charge(
    value: &SetValue,
    tracker: &TrackedExpressionAttributes,
) -> Result<usize, String> {
    const PLUS: usize = 34;
    const MINUS: usize = 37;
    Ok(match value {
        SetValue::Operand(operand) => operand_charge(operand, tracker)?,
        SetValue::Plus(a, b) => PLUS + operand_charge(a, tracker)? + operand_charge(b, tracker)?,
        SetValue::Minus(a, b) => MINUS + operand_charge(a, tracker)? + operand_charge(b, tracker)?,
    })
}

fn operand_charge(
    operand: &SetOperand,
    tracker: &TrackedExpressionAttributes,
) -> Result<usize, String> {
    const IF_NOT_EXISTS: usize = 39;
    const LIST_APPEND: usize = 37;
    Ok(match operand {
        SetOperand::Path(path) => path_operand_charge(path, tracker)?,
        SetOperand::ValueRef(name) => tracker.resolve_value(name)?.size(),
        SetOperand::IfNotExists(path, fallback) => {
            IF_NOT_EXISTS + path_operand_charge(path, tracker)? + operand_charge(fallback, tracker)?
        }
        SetOperand::ListAppend(a, b) => {
            LIST_APPEND + operand_charge(a, tracker)? + operand_charge(b, tracker)?
        }
        SetOperand::Group(inner) => set_value_charge(inner, tracker)?,
    })
}

/// Parse an UpdateExpression string.
pub fn parse(expr: &str) -> Result<UpdateExpr, String> {
    // Prefix the size error like every other UpdateExpression error; real
    // DynamoDB returns "Invalid UpdateExpression: Expression size has exceeded
    // the maximum allowed size". Confirmed against real DynamoDB (eu-west-2).
    super::check_expression_size(expr).map_err(|e| format!("Invalid UpdateExpression: {e}"))?;
    let tokens = tokenize(expr).map_err(|err| {
        // A character the tokenizer can't read (e.g. a stray `!`) is reported
        // in the same shape as a parser-level syntax error.
        format!(
            "Invalid UpdateExpression: {}",
            tokenizer_syntax_error(expr, &err)
        )
    })?;
    let mut stream = TokenStream::new(tokens);
    // Whitespace alone has no clause, and is refused at its end rather than
    // read as an update that changes nothing (captured eu-west-2 and
    // us-east-1, 2026-10-10). The caller refuses an empty string before this.
    if stream.at_end() {
        return Err(format!(
            "Invalid UpdateExpression: {}",
            stream.syntax_error(expr)
        ));
    }

    let mut set_actions = Vec::new();
    let mut remove_actions = Vec::new();
    let mut add_actions = Vec::new();
    let mut delete_actions = Vec::new();

    let mut seen_set = false;
    let mut seen_remove = false;
    let mut seen_add = false;
    let mut seen_delete = false;

    while !stream.at_end() {
        match stream.peek() {
            Some(Token::Set) => {
                if seen_set {
                    return Err("Invalid UpdateExpression: The \"SET\" section can only be used once in an update expression;".to_string());
                }
                seen_set = true;
                stream.next();
                parse_set_clause(&mut stream, &mut set_actions)
                    .map_err(|e| clause_error(expr, &stream, e))?;
            }
            Some(Token::Remove) => {
                if seen_remove {
                    return Err("Invalid UpdateExpression: The \"REMOVE\" section can only be used once in an update expression;".to_string());
                }
                seen_remove = true;
                stream.next();
                parse_remove_clause(&mut stream, &mut remove_actions)
                    .map_err(|e| clause_error(expr, &stream, e))?;
            }
            Some(Token::Add) => {
                if seen_add {
                    return Err("Invalid UpdateExpression: The \"ADD\" section can only be used once in an update expression;".to_string());
                }
                seen_add = true;
                stream.next();
                parse_add_clause(&mut stream, &mut add_actions)
                    .map_err(|e| clause_error(expr, &stream, e))?;
            }
            Some(Token::Delete) => {
                if seen_delete {
                    return Err("Invalid UpdateExpression: The \"DELETE\" section can only be used once in an update expression;".to_string());
                }
                seen_delete = true;
                stream.next();
                parse_delete_clause(&mut stream, &mut delete_actions)
                    .map_err(|e| clause_error(expr, &stream, e))?;
            }
            Some(_) => {
                // A token where SET, REMOVE, ADD or DELETE was required, at the
                // start or after a complete clause.
                return Err(format!(
                    "Invalid UpdateExpression: {}",
                    stream.syntax_error(expr)
                ));
            }
            None => break,
        }
    }

    Ok(UpdateExpr {
        set_actions,
        remove_actions,
        add_actions,
        delete_actions,
    })
}

/// A clause parser's error as DynamoDB reports it. A grammar failure ("Expected
/// ...") leaves the stream on the offending token and becomes the
/// `token: ..., near: ...` form; anything else keeps its own wording.
fn clause_error(expr: &str, stream: &TokenStream, err: String) -> String {
    if err.starts_with("Expected ") {
        format!("Invalid UpdateExpression: {}", stream.syntax_error(expr))
    } else {
        wrap_syntax_error(err)
    }
}

/// Wrap a sub-parser error with the standard syntax error prefix,
/// unless it already has a recognised higher-level prefix.
fn wrap_syntax_error(err: String) -> String {
    if err.starts_with("Invalid UpdateExpression:") {
        err
    } else if err.starts_with("Attribute name is a reserved keyword") {
        format!("Invalid UpdateExpression: {err}")
    } else {
        format!("Invalid UpdateExpression: Syntax error; {err}")
    }
}

/// Walk an UpdateExpr and track all attribute name and value references
/// without actually evaluating or modifying any item. This is used for
/// pre-validation: checking that all referenced names/values are defined,
/// and detecting unused names/values.
pub fn track_references(
    expr: &UpdateExpr,
    tracker: &TrackedExpressionAttributes,
) -> Result<(), String> {
    for action in &expr.set_actions {
        track_path_refs(&action.path, tracker)?;
        track_set_value_refs(&action.value, tracker)?;
    }
    for path in &expr.remove_actions {
        track_path_refs(path, tracker)?;
    }
    for action in &expr.add_actions {
        track_path_refs(&action.path, tracker)?;
        let val = tracker.resolve_value(&action.value_ref)?;
        // Validate ADD operand type statically
        validate_add_type(val)?;
    }
    for action in &expr.delete_actions {
        track_path_refs(&action.path, tracker)?;
        let val = tracker.resolve_value(&action.value_ref)?;
        // Validate DELETE operand type statically
        validate_delete_type(val)?;
    }

    // Static type validation for SET value expressions
    for action in &expr.set_actions {
        validate_set_value_types(&action.value, tracker)?;
    }

    check_target_overlaps(expr, tracker)
}

/// Refuse an update whose target paths overlap or conflict, as DynamoDB does
/// on UpdateItem and on a TransactWriteItems Update (captured eu-west-2 and
/// us-east-1, 2026-10-09). Names resolve through `tracker`; no value is read.
pub fn check_target_overlaps(
    expr: &UpdateExpr,
    tracker: &TrackedExpressionAttributes,
) -> Result<(), String> {
    let paths: Vec<Vec<PathElement>> = expr
        .set_actions
        .iter()
        .map(|action| &action.path)
        .chain(&expr.remove_actions)
        .chain(expr.add_actions.iter().map(|action| &action.path))
        .chain(expr.delete_actions.iter().map(|action| &action.path))
        .map(|path| resolve_tracked_path(path, tracker))
        .collect();
    check_path_overlaps(&paths).map_err(|message| format!("Invalid UpdateExpression: {message}"))
}

/// Validate that an ADD operand has a compatible type.
fn validate_add_type(val: &crate::types::AttributeValue) -> Result<(), String> {
    use crate::types::AttributeValue;
    match val {
        AttributeValue::N(_)
        | AttributeValue::SS(_)
        | AttributeValue::NS(_)
        | AttributeValue::BS(_) => Ok(()),
        _ => Err(format!(
            "Invalid UpdateExpression: Incorrect operand type for operator or function; \
             operator: ADD, operand type: {}, typeSet: ALLOWED_FOR_ADD_OPERAND",
            add_operand_type_name(val)
        )),
    }
}

/// Validate that a DELETE operand has a compatible type.
fn validate_delete_type(val: &crate::types::AttributeValue) -> Result<(), String> {
    use crate::types::AttributeValue;
    match val {
        AttributeValue::SS(_) | AttributeValue::NS(_) | AttributeValue::BS(_) => Ok(()),
        _ => Err(format!(
            "Invalid UpdateExpression: Incorrect operand type for operator or function; \
             operator: DELETE, operand type: {}, typeSet: ALLOWED_FOR_ADD_OPERAND",
            add_operand_type_name(val)
        )),
    }
}

/// How ADD's and DELETE's refusals name an operand's type: spelled out, with
/// binary as `Binary` (eu-west-2, 18 October 2026). The other operators use the
/// short codes of `AttributeValue::type_name`.
fn add_operand_type_name(val: &crate::types::AttributeValue) -> &'static str {
    use crate::types::AttributeValue;
    match val {
        AttributeValue::S(_) => "STRING",
        AttributeValue::N(_) => "NUMBER",
        AttributeValue::B(_) => "Binary",
        AttributeValue::BOOL(_) => "BOOL",
        AttributeValue::NULL(_) => "NULL",
        AttributeValue::SS(_) => "SS",
        AttributeValue::NS(_) => "NS",
        AttributeValue::BS(_) => "BS",
        AttributeValue::L(_) => "LIST",
        AttributeValue::M(_) => "MAP",
    }
}

/// Validate types for SET value expressions (arithmetic, list_append).
fn validate_set_value_types(
    value: &SetValue,
    tracker: &TrackedExpressionAttributes,
) -> Result<(), String> {
    match value {
        SetValue::Operand(op) => validate_set_operand_types(op, tracker),
        SetValue::Plus(left, right) => {
            validate_arithmetic_operand(left, "+", tracker)?;
            validate_arithmetic_operand(right, "+", tracker)
        }
        SetValue::Minus(left, right) => {
            validate_arithmetic_operand(left, "-", tracker)?;
            validate_arithmetic_operand(right, "-", tracker)
        }
    }
}

/// Validate that an operand used in + or - is a number (if it's a value ref).
fn validate_arithmetic_operand(
    operand: &SetOperand,
    op: &str,
    tracker: &TrackedExpressionAttributes,
) -> Result<(), String> {
    use crate::types::AttributeValue;
    match operand {
        SetOperand::ValueRef(name) => {
            let val = tracker.resolve_value(name)?;
            if !matches!(val, AttributeValue::N(_)) {
                return Err(format!(
                    "Invalid UpdateExpression: Incorrect operand type for operator or function; \
                     operator or function: {op}, operand type: {}",
                    val.type_name()
                ));
            }
            Ok(())
        }
        SetOperand::IfNotExists(_, default) => validate_set_operand_types(default, tracker),
        SetOperand::ListAppend(a, b) => {
            validate_list_append_operand(a, tracker)?;
            validate_list_append_operand(b, tracker)
        }
        SetOperand::Path(_) => Ok(()), // Path types checked at runtime
        // A parenthesised group resolves to a number at runtime; validate its
        // inner expression but leave the numeric check to evaluation.
        SetOperand::Group(inner) => validate_set_value_types(inner, tracker),
    }
}

/// Validate types for a set operand (recursively).
fn validate_set_operand_types(
    operand: &SetOperand,
    tracker: &TrackedExpressionAttributes,
) -> Result<(), String> {
    match operand {
        SetOperand::ListAppend(a, b) => {
            validate_list_append_operand(a, tracker)?;
            validate_list_append_operand(b, tracker)
        }
        SetOperand::IfNotExists(_, default) => validate_set_operand_types(default, tracker),
        SetOperand::Group(inner) => validate_set_value_types(inner, tracker),
        _ => Ok(()),
    }
}

/// Validate a list_append operand is a list if it's a value ref.
fn validate_list_append_operand(
    operand: &SetOperand,
    tracker: &TrackedExpressionAttributes,
) -> Result<(), String> {
    use crate::types::AttributeValue;
    if let SetOperand::ValueRef(name) = operand {
        let val = tracker.resolve_value(name)?;
        if !matches!(val, AttributeValue::L(_)) {
            return Err(format!(
                "Invalid UpdateExpression: Incorrect operand type for operator or function; \
                 operator or function: list_append, operand type: {}",
                val.type_name()
            ));
        }
    }
    Ok(())
}

/// Resolve path elements to their final names (expanding #name refs).
fn resolve_tracked_path(
    path: &[PathElement],
    tracker: &TrackedExpressionAttributes,
) -> Vec<PathElement> {
    path.iter()
        .map(|elem| {
            if let PathElement::Attribute(name) = elem
                && name.starts_with('#')
                && let Ok(resolved) = tracker.resolve_name(name)
            {
                return PathElement::Attribute(resolved);
            }
            elem.clone()
        })
        .collect()
}

/// Check for overlapping or conflicting document paths, naming the two in the
/// order they were written (captured eu-west-2 and us-east-1, 2026-10-09).
///
/// Two paths overlap if one is a prefix of the other (e.g., `a.b` and `a.b.c`).
/// Two paths conflict if they share elements but diverge in type at the same
/// position (e.g., `a[3].c` and `a.c[3]`).
/// Two paths of one update that overlap, in DynamoDB's words without the
/// `Invalid UpdateExpression: ` lead that UpdateItem puts in front. The earlier
/// of the two in `paths` is `path one`.
pub(crate) fn check_path_overlaps(paths: &[Vec<PathElement>]) -> Result<(), String> {
    for i in 0..paths.len() {
        for j in (i + 1)..paths.len() {
            let a = &paths[i];
            let b = &paths[j];
            let min_len = a.len().min(b.len());

            // Check common prefix length
            let mut common = 0;
            for k in 0..min_len {
                if a[k] == b[k] {
                    common += 1;
                } else {
                    break;
                }
            }

            if common == 0 {
                continue;
            }

            // If one path is a prefix of the other, they overlap
            if common == a.len() || common == b.len() {
                return Err(format!(
                    "Two document paths overlap with each other; \
                     must remove or rewrite one of these paths; \
                     path one: {}, path two: {}",
                    format_path_for_error(a),
                    format_path_for_error(b)
                ));
            }

            // If paths share a prefix but diverge, they conflict
            if common > 0 && common < min_len && a == b {
                return Err(format!(
                    "Two document paths conflict with each other; \
                     must remove or rewrite one of these paths; \
                     path one: {}, path two: {}",
                    format_path_for_error(a),
                    format_path_for_error(b)
                ));
            }
        }
    }
    Ok(())
}

fn track_path_refs(
    path: &[PathElement],
    tracker: &TrackedExpressionAttributes,
) -> Result<(), String> {
    for elem in path {
        if let PathElement::Attribute(name) = elem
            && name.starts_with('#')
        {
            tracker.resolve_name(name)?;
        }
    }
    Ok(())
}

fn track_set_value_refs(
    value: &SetValue,
    tracker: &TrackedExpressionAttributes,
) -> Result<(), String> {
    match value {
        SetValue::Operand(op) => track_set_operand_refs(op, tracker),
        SetValue::Plus(left, right) | SetValue::Minus(left, right) => {
            track_set_operand_refs(left, tracker)?;
            track_set_operand_refs(right, tracker)
        }
    }
}

fn track_set_operand_refs(
    operand: &SetOperand,
    tracker: &TrackedExpressionAttributes,
) -> Result<(), String> {
    match operand {
        SetOperand::Path(path) => track_path_refs(path, tracker),
        SetOperand::ValueRef(name) => {
            tracker.resolve_value(name)?;
            Ok(())
        }
        SetOperand::IfNotExists(path, default) => {
            track_path_refs(path, tracker)?;
            track_set_operand_refs(default, tracker)
        }
        SetOperand::ListAppend(a, b) => {
            track_set_operand_refs(a, tracker)?;
            track_set_operand_refs(b, tracker)
        }
        SetOperand::Group(inner) => track_set_value_refs(inner, tracker),
    }
}

/// Apply an update expression to an item (mutating it in place), tracking attribute usage.
pub fn apply(
    item: &mut HashMap<String, AttributeValue>,
    expr: &UpdateExpr,
    tracker: &TrackedExpressionAttributes,
) -> Result<(), String> {
    // Process SET actions.
    //
    // Every SET right-hand side is evaluated against the pre-update image, so
    // that `SET a = :v, b = a` gives `b` the OLD value of `a` rather than the
    // value assigned to `a` earlier in the same expression. DynamoDB applies
    // the whole expression to the item as it appeared before the update, so all
    // reads see the original snapshot. (Overlapping target paths are rejected by
    // `check_path_overlaps`, so no SET can legitimately read another's output.)
    let snapshot = item.clone();

    // An ADD or DELETE whose operand does not fit what is stored is refused
    // before any path is applied, so it wins over a bad path in a SET or REMOVE
    // of the same expression (eu-west-2, 18 October 2026).
    for action in &expr.add_actions {
        let path = resolve_path_elements(&action.path, tracker)?;
        let add_val = tracker.resolve_value(&action.value_ref)?;
        if !add_fits(resolve_path(&snapshot, &path).as_ref(), add_val) {
            return Err(OPERAND_TYPE.to_string());
        }
    }
    for action in &expr.delete_actions {
        let path = resolve_path_elements(&action.path, tracker)?;
        let del_val = tracker.resolve_value(&action.value_ref)?;
        if !delete_fits(resolve_path(&snapshot, &path).as_ref(), del_val) {
            return Err(OPERAND_TYPE.to_string());
        }
    }

    let mut writes = Vec::with_capacity(expr.set_actions.len());
    for action in &expr.set_actions {
        let resolved_path = resolve_path_elements(&action.path, tracker)?;
        let value = evaluate_set_value(&action.value, &snapshot, tracker)?;
        writes.push((resolved_path, value));
    }
    // A SET past the end of a list appends, and two of them on the same list
    // append in the order of their indexes rather than of their clauses
    // (eu-west-2, 9 October 2026). Every value is already worked out from the
    // snapshot and no two targets overlap, so writing in path order changes
    // nothing else.
    writes.sort_by(|(a, _), (b, _)| compare_paths(a, b));
    for (path, value) in writes {
        set_path(item, &path, value)?;
    }

    // Process ADD actions
    for action in &expr.add_actions {
        let resolved_path = resolve_path_elements(&action.path, tracker)?;
        let add_val = tracker.resolve_value(&action.value_ref)?.clone();
        apply_add(item, &resolved_path, &add_val).map_err(operand_or_path_error)?;
    }

    // Process DELETE actions
    for action in &expr.delete_actions {
        let resolved_path = resolve_path_elements(&action.path, tracker)?;
        let del_val = tracker.resolve_value(&action.value_ref)?.clone();
        apply_delete(item, &resolved_path, &del_val).map_err(operand_or_path_error)?;
    }

    // REMOVE goes last, highest path first. Every path in an expression means
    // what it meant in the item as it was, so taking out `l[0]` must not move
    // the element that `l[1]` or `l[1].x` names, and an ADD or DELETE on
    // either has already run against the original indexes (eu-west-2,
    // 18 October 2026).
    let mut removals = Vec::with_capacity(expr.remove_actions.len());
    for path in &expr.remove_actions {
        removals.push(resolve_path_elements(path, tracker)?);
    }
    removals.sort_by(|a, b| compare_paths(b, a));
    for path in removals {
        remove_path(item, &path)?;
    }

    Ok(())
}

/// An ADD or DELETE failure as the caller sees it: a bad path says so, anything
/// else is an operand that does not fit what is stored.
fn operand_or_path_error(error: String) -> String {
    if error == INVALID_UPDATE_PATH {
        error
    } else {
        OPERAND_TYPE.to_string()
    }
}

/// Whether ADD can apply `add_val` to `existing`: a number to a number, a set
/// to a set of its kind, a list to a list, any of these to an absent attribute.
fn add_fits(existing: Option<&AttributeValue>, add_val: &AttributeValue) -> bool {
    use AttributeValue::{BS, L, N, NS, SS};
    matches!(
        (existing, add_val),
        (Some(N(_)) | None, N(_))
            | (Some(SS(_)) | None, SS(_))
            | (Some(NS(_)) | None, NS(_))
            | (Some(BS(_)) | None, BS(_))
            | (Some(L(_)) | None, L(_))
    )
}

/// Whether DELETE can apply `del_val` to `existing`: a set to a set of its
/// kind, or anything to an absent attribute, which it leaves alone.
fn delete_fits(existing: Option<&AttributeValue>, del_val: &AttributeValue) -> bool {
    use AttributeValue::{BS, NS, SS};
    matches!(
        (existing, del_val),
        (None, _) | (Some(SS(_)), SS(_)) | (Some(NS(_)), NS(_)) | (Some(BS(_)), BS(_))
    )
}

/// Order two resolved paths element by element, with list indexes compared as
/// numbers.
pub(crate) fn compare_paths(a: &[PathElement], b: &[PathElement]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (x, y) in a.iter().zip(b) {
        let order = match (x, y) {
            (PathElement::Attribute(x), PathElement::Attribute(y)) => x.cmp(y),
            (PathElement::Index(x), PathElement::Index(y)) => x.cmp(y),
            (PathElement::Attribute(_), PathElement::Index(_)) => Ordering::Less,
            (PathElement::Index(_), PathElement::Attribute(_)) => Ordering::Greater,
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    a.len().cmp(&b.len())
}

// ---------------------------------------------------------------------------
// SET value evaluation
// ---------------------------------------------------------------------------

fn evaluate_set_value(
    value: &SetValue,
    item: &HashMap<String, AttributeValue>,
    tracker: &TrackedExpressionAttributes,
) -> Result<AttributeValue, String> {
    match value {
        SetValue::Operand(op) => evaluate_set_operand(op, item, tracker),
        SetValue::Plus(left, right) => {
            let lv = evaluate_set_operand(left, item, tracker)?;
            let rv = evaluate_set_operand(right, item, tracker)?;
            match (&lv, &rv) {
                (AttributeValue::N(a), AttributeValue::N(b)) => {
                    use bigdecimal::BigDecimal;
                    use std::str::FromStr;
                    let da = BigDecimal::from_str(a).map_err(|_| format!("Invalid number: {a}"))?;
                    let db = BigDecimal::from_str(b).map_err(|_| format!("Invalid number: {b}"))?;
                    let result = &da + &db;
                    Ok(AttributeValue::N(format_number(&result)))
                }
                _ => Err(OPERAND_TYPE.to_string()),
            }
        }
        SetValue::Minus(left, right) => {
            let lv = evaluate_set_operand(left, item, tracker)?;
            let rv = evaluate_set_operand(right, item, tracker)?;
            match (&lv, &rv) {
                (AttributeValue::N(a), AttributeValue::N(b)) => {
                    use bigdecimal::BigDecimal;
                    use std::str::FromStr;
                    let da = BigDecimal::from_str(a).map_err(|_| format!("Invalid number: {a}"))?;
                    let db = BigDecimal::from_str(b).map_err(|_| format!("Invalid number: {b}"))?;
                    let result = &da - &db;
                    Ok(AttributeValue::N(format_number(&result)))
                }
                _ => Err(OPERAND_TYPE.to_string()),
            }
        }
    }
}

fn evaluate_set_operand(
    operand: &SetOperand,
    item: &HashMap<String, AttributeValue>,
    tracker: &TrackedExpressionAttributes,
) -> Result<AttributeValue, String> {
    match operand {
        SetOperand::Path(path) => {
            let resolved = resolve_path_elements(path, tracker)?;
            resolve_path(item, &resolved).ok_or_else(|| MISSING_ATTRIBUTE.to_string())
        }
        SetOperand::ValueRef(name) => Ok(tracker.resolve_value(name)?.clone()),
        SetOperand::IfNotExists(path, default) => {
            let resolved = resolve_path_elements(path, tracker)?;
            match resolve_path(item, &resolved) {
                Some(existing) => Ok(existing),
                None => evaluate_set_operand(default, item, tracker),
            }
        }
        SetOperand::ListAppend(list1, list2) => {
            let v1 = evaluate_set_operand(list1, item, tracker)?;
            let v2 = evaluate_set_operand(list2, item, tracker)?;
            match (v1, v2) {
                (AttributeValue::L(mut a), AttributeValue::L(b)) => {
                    a.extend(b);
                    Ok(AttributeValue::L(a))
                }
                _ => Err(OPERAND_TYPE.to_string()),
            }
        }
        SetOperand::Group(inner) => evaluate_set_value(inner, item, tracker),
    }
}

// ---------------------------------------------------------------------------
// ADD action
// ---------------------------------------------------------------------------

/// Public wrapper for use by legacy `AttributeUpdates` support.
pub fn apply_add_public(
    item: &mut HashMap<String, AttributeValue>,
    path: &[PathElement],
    add_val: &AttributeValue,
) -> Result<(), String> {
    apply_add(item, path, add_val)
}

fn apply_add(
    item: &mut HashMap<String, AttributeValue>,
    path: &[PathElement],
    add_val: &AttributeValue,
) -> Result<(), String> {
    let existing = resolve_path(item, path);

    match (existing, add_val) {
        // Number: add to existing number or create
        (Some(AttributeValue::N(existing_n)), AttributeValue::N(add_n)) => {
            use bigdecimal::BigDecimal;
            use std::str::FromStr;
            let de = BigDecimal::from_str(&existing_n)
                .map_err(|_| format!("Invalid number: {existing_n}"))?;
            let da = BigDecimal::from_str(add_n).map_err(|_| format!("Invalid number: {add_n}"))?;
            let result = &de + &da;
            set_path(item, path, AttributeValue::N(format_number(&result)))
        }
        (None, AttributeValue::N(_)) => {
            // Create with the provided value
            set_path(item, path, add_val.clone())
        }

        // String set: union
        (Some(AttributeValue::SS(mut existing_set)), AttributeValue::SS(add_set)) => {
            for s in add_set {
                if !existing_set.contains(s) {
                    existing_set.push(s.clone());
                }
            }
            set_path(item, path, AttributeValue::SS(existing_set))
        }
        (None, AttributeValue::SS(_)) => set_path(item, path, add_val.clone()),

        // Number set: union
        (Some(AttributeValue::NS(mut existing_set)), AttributeValue::NS(add_set)) => {
            for n in add_set {
                if !existing_set.contains(n) {
                    existing_set.push(n.clone());
                }
            }
            set_path(item, path, AttributeValue::NS(existing_set))
        }
        (None, AttributeValue::NS(_)) => set_path(item, path, add_val.clone()),

        // Binary set: union
        (Some(AttributeValue::BS(mut existing_set)), AttributeValue::BS(add_set)) => {
            for b in add_set {
                if !existing_set.contains(b) {
                    existing_set.push(b.clone());
                }
            }
            set_path(item, path, AttributeValue::BS(existing_set))
        }
        (None, AttributeValue::BS(_)) => set_path(item, path, add_val.clone()),

        // List: append elements (legacy AttributeUpdates behaviour)
        (Some(AttributeValue::L(mut existing_list)), AttributeValue::L(add_list)) => {
            existing_list.extend(add_list.iter().cloned());
            set_path(item, path, AttributeValue::L(existing_list))
        }
        (None, AttributeValue::L(_)) => set_path(item, path, add_val.clone()),

        _ => Err("Type mismatch for attribute to update".to_string()),
    }
}

// ---------------------------------------------------------------------------
// DELETE action
// ---------------------------------------------------------------------------

/// Public wrapper for use by legacy `AttributeUpdates` support.
pub fn apply_delete_public(
    item: &mut HashMap<String, AttributeValue>,
    path: &[PathElement],
    del_val: &AttributeValue,
) -> Result<(), String> {
    apply_delete(item, path, del_val)
}

fn apply_delete(
    item: &mut HashMap<String, AttributeValue>,
    path: &[PathElement],
    del_val: &AttributeValue,
) -> Result<(), String> {
    check_update_parent(item, path)?;
    let existing = resolve_path(item, path);

    match (existing, del_val) {
        (Some(AttributeValue::SS(existing_set)), AttributeValue::SS(del_set)) => {
            let new_set: Vec<String> = existing_set
                .into_iter()
                .filter(|s| !del_set.contains(s))
                .collect();
            if new_set.is_empty() {
                remove_path(item, path)
            } else {
                set_path(item, path, AttributeValue::SS(new_set))
            }
        }
        (Some(AttributeValue::NS(existing_set)), AttributeValue::NS(del_set)) => {
            let new_set: Vec<String> = existing_set
                .into_iter()
                .filter(|n| !del_set.contains(n))
                .collect();
            if new_set.is_empty() {
                remove_path(item, path)
            } else {
                set_path(item, path, AttributeValue::NS(new_set))
            }
        }
        (Some(AttributeValue::BS(existing_set)), AttributeValue::BS(del_set)) => {
            let new_set: Vec<Vec<u8>> = existing_set
                .into_iter()
                .filter(|b| !del_set.contains(b))
                .collect();
            if new_set.is_empty() {
                remove_path(item, path)
            } else {
                set_path(item, path, AttributeValue::BS(new_set))
            }
        }
        (None, _) => Ok(()), // Nothing to delete from
        _ => Err("Type mismatch for attribute to update".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

fn parse_set_clause(stream: &mut TokenStream, actions: &mut Vec<SetAction>) -> Result<(), String> {
    actions.push(parse_set_action(stream)?);
    while matches!(stream.peek(), Some(Token::Comma)) {
        stream.next();
        actions.push(parse_set_action(stream)?);
    }
    Ok(())
}

fn parse_set_action(stream: &mut TokenStream) -> Result<SetAction, String> {
    let path = parse_raw_path(stream)?;
    stream.expect(&Token::Eq)?;
    let value = parse_set_value(stream)?;
    Ok(SetAction { path, value })
}

fn parse_set_value(stream: &mut TokenStream) -> Result<SetValue, String> {
    let left = parse_set_operand(stream)?;

    match stream.peek() {
        Some(Token::Plus) => {
            stream.next();
            let right = parse_set_operand(stream)?;
            Ok(SetValue::Plus(left, right))
        }
        Some(Token::Minus) => {
            stream.next();
            let right = parse_set_operand(stream)?;
            Ok(SetValue::Minus(left, right))
        }
        _ => Ok(SetValue::Operand(left)),
    }
}

fn parse_set_operand(stream: &mut TokenStream) -> Result<SetOperand, String> {
    // Check for functions: if_not_exists, list_append
    if let Some(Token::Identifier(name)) = stream.peek() {
        let func_name = name.to_lowercase();
        let orig_name = name.clone();
        match func_name.as_str() {
            "if_not_exists" => {
                stream.next();
                stream.expect(&Token::LParen)?;

                // First argument must be a document path (not a value ref or function)
                match stream.peek() {
                    Some(Token::ValueRef(_)) => {
                        return Err(
                            "Invalid UpdateExpression: Operator or function requires a document path; \
                             operator or function: if_not_exists".to_string()
                        );
                    }
                    Some(Token::Identifier(fname))
                        if fname.to_lowercase() == "if_not_exists"
                            || fname.to_lowercase() == "list_append" =>
                    {
                        return Err(
                            "Invalid UpdateExpression: Operator or function requires a document path; \
                             operator or function: if_not_exists".to_string()
                        );
                    }
                    _ => {}
                }

                let path = parse_raw_path(stream)?;

                // Check for correct number of operands
                if !matches!(stream.peek(), Some(Token::Comma)) {
                    return Err(
                        "Invalid UpdateExpression: Incorrect number of operands for operator or function; \
                         operator or function: if_not_exists, number of operands: 1".to_string()
                    );
                }
                stream.expect(&Token::Comma)?;
                let default = parse_set_operand(stream)?;
                stream.expect(&Token::RParen)?;
                return Ok(SetOperand::IfNotExists(path, Box::new(default)));
            }
            "list_append" => {
                stream.next();
                stream.expect(&Token::LParen)?;
                let list1 = parse_set_operand(stream)?;

                // Check for correct number of operands
                if !matches!(stream.peek(), Some(Token::Comma)) {
                    return Err(
                        "Invalid UpdateExpression: Incorrect number of operands for operator or function; \
                         operator or function: list_append, number of operands: 1".to_string()
                    );
                }
                stream.expect(&Token::Comma)?;
                let list2 = parse_set_operand(stream)?;
                stream.expect(&Token::RParen)?;
                return Ok(SetOperand::ListAppend(Box::new(list1), Box::new(list2)));
            }
            _ => {
                // Check if this looks like a function call (identifier followed by '(')
                // If so, report "Invalid function name" for unknown functions.
                let saved_pos = stream.pos();
                stream.next();
                if matches!(stream.peek(), Some(Token::LParen)) {
                    return Err(format!(
                        "Invalid UpdateExpression: Invalid function name; function: {}",
                        orig_name
                    ));
                }
                // Rewind: not a function call, treat as path
                stream.set_pos(saved_pos);
            }
        }
    }

    match stream.peek() {
        // Parenthesised sub-expression, e.g. `(c - :v)`. The contents are a full
        // SET value (operand or arithmetic), evaluated on the same BigDecimal path.
        Some(Token::LParen) => {
            stream.next();
            let inner = parse_set_value(stream)?;
            stream.expect(&Token::RParen)?;
            Ok(SetOperand::Group(Box::new(inner)))
        }
        Some(Token::ValueRef(_)) => {
            if let Some(Token::ValueRef(name)) = stream.next().cloned() {
                Ok(SetOperand::ValueRef(name))
            } else {
                unreachable!()
            }
        }
        Some(Token::Identifier(_)) | Some(Token::NameRef(_)) => {
            let path = parse_raw_path(stream)?;
            Ok(SetOperand::Path(path))
        }
        Some(t) => Err(format!("Expected operand in SET, got {t}")),
        None => Err("Expected operand in SET, got end of expression".to_string()),
    }
}

fn parse_remove_clause(
    stream: &mut TokenStream,
    actions: &mut Vec<Vec<PathElement>>,
) -> Result<(), String> {
    actions.push(parse_raw_path(stream)?);
    while matches!(stream.peek(), Some(Token::Comma)) {
        stream.next();
        actions.push(parse_raw_path(stream)?);
    }
    Ok(())
}

fn parse_add_clause(stream: &mut TokenStream, actions: &mut Vec<AddAction>) -> Result<(), String> {
    actions.push(parse_add_action(stream)?);
    while matches!(stream.peek(), Some(Token::Comma)) {
        stream.next();
        actions.push(parse_add_action(stream)?);
    }
    Ok(())
}

fn parse_add_action(stream: &mut TokenStream) -> Result<AddAction, String> {
    let path = parse_raw_path(stream)?;
    let value_ref = match stream.peek() {
        Some(Token::ValueRef(name)) => name.clone(),
        Some(t) => return Err(format!("Expected value reference in ADD, got {t}")),
        None => return Err("Expected value reference in ADD, got end of expression".to_string()),
    };
    stream.next();
    Ok(AddAction { path, value_ref })
}

fn parse_delete_clause(
    stream: &mut TokenStream,
    actions: &mut Vec<DeleteAction>,
) -> Result<(), String> {
    actions.push(parse_delete_action(stream)?);
    while matches!(stream.peek(), Some(Token::Comma)) {
        stream.next();
        actions.push(parse_delete_action(stream)?);
    }
    Ok(())
}

fn parse_delete_action(stream: &mut TokenStream) -> Result<DeleteAction, String> {
    let path = parse_raw_path(stream)?;
    let value_ref = match stream.peek() {
        Some(Token::ValueRef(name)) => name.clone(),
        Some(t) => return Err(format!("Expected value reference in DELETE, got {t}")),
        None => return Err("Expected value reference in DELETE, got end of expression".to_string()),
    };
    stream.next();
    Ok(DeleteAction { path, value_ref })
}

/// Format a BigDecimal number, stripping unnecessary trailing zeros.
/// DynamoDB returns numbers without scientific notation.
fn format_number(n: &bigdecimal::BigDecimal) -> String {
    let normalized = n.normalized();
    // Force scale >= 0 so BigDecimal renders without scientific notation.
    // When the exponent is negative (large integer like 1e38), with_scale(0)
    // expands to full decimal digits.
    if normalized.as_bigint_and_exponent().1 < 0 {
        normalized.with_scale(0).to_string()
    } else {
        normalized.to_string()
    }
}

#[cfg(test)]
mod charge_tests {
    use super::parse;
    use crate::expressions::TrackedExpressionAttributes;
    use crate::types::AttributeValue;
    use std::collections::HashMap;

    fn s(len: usize) -> AttributeValue {
        AttributeValue::S("x".repeat(len))
    }

    fn values() -> HashMap<String, AttributeValue> {
        let list = |items: Vec<AttributeValue>| AttributeValue::L(items);
        HashMap::from([
            (":v1".to_string(), s(1)),
            (":v10".to_string(), s(10)),
            (":s100".to_string(), s(100)),
            (":one".to_string(), AttributeValue::N("1".to_string())),
            (
                ":big".to_string(),
                AttributeValue::N("12345678901234567890".to_string()),
            ),
            (":lst".to_string(), list(vec![s(10)])),
            (":empty".to_string(), list(vec![])),
            (":b100".to_string(), AttributeValue::B(vec![7; 100])),
            (":true".to_string(), AttributeValue::BOOL(true)),
            (":null".to_string(), AttributeValue::NULL(true)),
            (
                ":ns".to_string(),
                AttributeValue::NS(vec!["1".to_string(), "22".to_string(), "333".to_string()]),
            ),
            (
                ":bs".to_string(),
                AttributeValue::BS(vec![vec![1; 100], vec![2; 50]]),
            ),
            (
                ":map".to_string(),
                AttributeValue::M(HashMap::from([("a".to_string(), s(100))])),
            ),
            (
                ":mixed".to_string(),
                list(vec![s(100), AttributeValue::N("5".to_string())]),
            ),
        ])
    }

    fn names() -> HashMap<String, String> {
        HashMap::from([
            ("#t".to_string(), "abcdefghij".to_string()),
            ("#b".to_string(), "b".to_string()),
            ("#e".to_string(), "\u{20ac}\u{20ac}".to_string()),
            ("#a".to_string(), "\u{e9}".to_string()),
            ("#m".to_string(), "\u{1f600}".to_string()),
        ])
    }

    /// The charge of one statement, resolving only the names and values it uses.
    fn charge(expr: &str) -> usize {
        let names = Some(names());
        let values = Some(values());
        let tracker = TrackedExpressionAttributes::without_tracking(&names, &values);
        parse(expr)
            .unwrap_or_else(|e| panic!("parse {expr:?}: {e}"))
            .request_charge(&tracker)
            .unwrap_or_else(|e| panic!("charge {expr:?}: {e}"))
    }

    /// Every form the UpdateExpression grammar allows, at the cost eu-west-2
    /// charged for it on 9 October 2026. Each figure is the whole statement:
    /// three for the statement itself plus its clauses.
    #[test]
    fn each_form_is_charged_at_its_captured_cost() {
        let cases: &[(&str, usize)] = &[
            // Clauses, with a value operand.
            ("SET b = :v1", 3 + 19 + 1 + 1),
            ("SET #b = :v1", 3 + 19 + 1 + 1),
            ("SET b = :v1, c = :v1", 3 + 2 * (19 + 1 + 1)),
            ("ADD n :one", 3 + 19 + 1 + 2),
            ("ADD abcdefghij :one", 3 + 19 + 10 + 2),
            ("ADD m.n :one", 3 + 19 + 6 + 2),
            ("REMOVE r", 3 + 1 + 1),
            ("REMOVE abcdefghij", 3 + 1 + 10),
            ("DELETE ss :v10", 3 + 1 + 2),
            ("DELETE abcdefghij :v10", 3 + 1 + 10),
            ("DELETE m.ss :v10", 3 + 1 + 7),
            // Targets: the first element, then 4 plus each later element.
            ("SET l[0] = :v10", 3 + 19 + 6 + 10),
            ("SET l[10] = :v10", 3 + 19 + 7 + 10),
            ("SET m.a.x = :v10", 3 + 19 + 11 + 10),
            ("SET mmmmm.xxxxx = :v10", 3 + 19 + 14 + 10),
            ("SET l[0].x = :v10", 3 + 19 + 11 + 10),
            ("REMOVE l[1]", 3 + 1 + 6),
            ("REMOVE l[10]", 3 + 1 + 7),
            ("REMOVE m.y", 3 + 1 + 6),
            ("REMOVE m.a.y", 3 + 1 + 11),
            ("REMOVE abcdefghij[1]", 3 + 1 + 15),
            // A name counts its UTF-8 bytes.
            ("SET #e = :v10", 3 + 19 + 6 + 10),
            ("SET #a = :v10", 3 + 19 + 2 + 10),
            ("SET #m = :v10", 3 + 19 + 4 + 10),
            ("SET m.#e = :v10", 3 + 19 + 11 + 10),
            // Path operands: 1, then 14 plus each element; the value read is free.
            ("SET c = big", 3 + 19 + 1 + 18),
            ("SET c = abcdefghij", 3 + 19 + 1 + 25),
            ("SET c = #t", 3 + 19 + 1 + 25),
            ("SET c = #e", 3 + 19 + 1 + 21),
            ("SET c = m.z", 3 + 19 + 1 + 31),
            ("SET c = m.a.z", 3 + 19 + 1 + 46),
            ("SET c = l[0]", 3 + 19 + 1 + 31),
            ("SET c = l[10]", 3 + 19 + 1 + 32),
            // Arithmetic, and parentheses, which cost nothing.
            ("SET n = n + :one", 3 + 19 + 1 + 34 + 16 + 2),
            ("SET n = :one + n", 3 + 19 + 1 + 34 + 16 + 2),
            ("SET n = (n + :one)", 3 + 19 + 1 + 34 + 16 + 2),
            ("SET n = n + :big", 3 + 19 + 1 + 34 + 16 + 11),
            ("SET n = n + n", 3 + 19 + 1 + 34 + 16 + 16),
            ("SET n = :v1 + :v10", 3 + 19 + 1 + 34 + 1 + 10),
            ("SET n = n - :one", 3 + 19 + 1 + 37 + 16 + 2),
            ("SET n = :v1 - :v10", 3 + 19 + 1 + 37 + 1 + 10),
            ("SET #t = #t + :one", 3 + 19 + 10 + 34 + 25 + 2),
            // if_not_exists and list_append.
            ("SET c = if_not_exists(c, :v1)", 3 + 19 + 1 + 39 + 16 + 1),
            ("SET l = list_append(l, :lst)", 3 + 19 + 1 + 37 + 16 + 14),
            ("SET l = list_append(:lst, l)", 3 + 19 + 1 + 37 + 14 + 16),
            (
                "SET l = list_append(:empty, :lst)",
                3 + 19 + 1 + 37 + 3 + 14,
            ),
            (
                "SET l = list_append(if_not_exists(l, :empty), :lst)",
                3 + 19 + 1 + 37 + (39 + 16 + 3) + 14,
            ),
            // Every value type costs its size, at every use.
            (
                "SET c = if_not_exists(c, :s100)",
                3 + 19 + 1 + 39 + 16 + 100,
            ),
            (
                "SET c = if_not_exists(c, :b100)",
                3 + 19 + 1 + 39 + 16 + 100,
            ),
            ("SET c = if_not_exists(c, :true)", 3 + 19 + 1 + 39 + 16 + 1),
            ("SET c = if_not_exists(c, :null)", 3 + 19 + 1 + 39 + 16 + 1),
            ("SET c = if_not_exists(c, :ns)", 3 + 19 + 1 + 39 + 16 + 7),
            ("SET c = if_not_exists(c, :bs)", 3 + 19 + 1 + 39 + 16 + 150),
            ("SET c = if_not_exists(c, :map)", 3 + 19 + 1 + 39 + 16 + 105),
            (
                "SET c = if_not_exists(c, :mixed)",
                3 + 19 + 1 + 39 + 16 + 107,
            ),
            (
                "SET c = if_not_exists(c, :s100), d = if_not_exists(d, :s100)",
                3 + 2 * (19 + 1 + 39 + 16 + 100),
            ),
            // Clauses add up.
            (
                "SET c = :v10 ADD n :one REMOVE r DELETE ss :v1",
                3 + (19 + 1 + 10) + (19 + 1 + 2) + (1 + 1) + (1 + 2),
            ),
        ];
        let mut wrong = Vec::new();
        for (expr, expected) in cases {
            let got = charge(expr);
            if got != *expected {
                wrong.push(format!(
                    "{expr:?}: charged {got}, eu-west-2 charges {expected}"
                ));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} of {} forms charged wrongly:\n{}",
            wrong.len(),
            cases.len(),
            wrong.join("\n")
        );
    }

    #[test]
    fn an_unresolved_name_or_value_is_an_error_rather_than_free() {
        let names = None;
        let values = None;
        let tracker = TrackedExpressionAttributes::without_tracking(&names, &values);
        assert!(
            parse("SET b = :v")
                .unwrap()
                .request_charge(&tracker)
                .is_err()
        );
        assert!(
            parse("SET #b = b")
                .unwrap()
                .request_charge(&tracker)
                .is_err()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One value of every type, for crossing with every stored type.
    fn every_type() -> Vec<AttributeValue> {
        vec![
            AttributeValue::S("x".into()),
            AttributeValue::N("1".into()),
            AttributeValue::B(vec![1]),
            AttributeValue::BOOL(true),
            AttributeValue::NULL(true),
            AttributeValue::L(vec![AttributeValue::N("1".into())]),
            AttributeValue::M(HashMap::new()),
            AttributeValue::SS(vec!["a".into()]),
            AttributeValue::NS(vec!["1".into()]),
            AttributeValue::BS(vec![vec![1]]),
        ]
    }

    /// The up-front type check and the applier read one table: whatever
    /// `add_fits` lets through, `apply_add` applies, and the other way round,
    /// for an absent attribute and for every stored type.
    #[test]
    fn the_add_type_check_agrees_with_the_applier() {
        let path = [PathElement::Attribute("a".into())];
        let mut stored = vec![None];
        stored.extend(every_type().into_iter().map(Some));
        for existing in &stored {
            for operand in every_type() {
                let mut item = HashMap::new();
                if let Some(value) = existing {
                    item.insert("a".to_string(), value.clone());
                }
                assert_eq!(
                    add_fits(existing.as_ref(), &operand),
                    apply_add(&mut item, &path, &operand).is_ok(),
                    "ADD {operand:?} to {existing:?}"
                );
            }
        }
    }

    #[test]
    fn the_delete_type_check_agrees_with_the_applier() {
        let path = [PathElement::Attribute("a".into())];
        let mut stored = vec![None];
        stored.extend(every_type().into_iter().map(Some));
        for existing in &stored {
            for operand in every_type() {
                let mut item = HashMap::new();
                if let Some(value) = existing {
                    item.insert("a".to_string(), value.clone());
                }
                assert_eq!(
                    delete_fits(existing.as_ref(), &operand),
                    apply_delete(&mut item, &path, &operand).is_ok(),
                    "DELETE {operand:?} from {existing:?}"
                );
            }
        }
    }

    fn make_item(pairs: &[(&str, AttributeValue)]) -> HashMap<String, AttributeValue> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn vals(pairs: &[(&str, AttributeValue)]) -> Option<HashMap<String, AttributeValue>> {
        Some(make_item(pairs))
    }

    fn make_tracker<'a>(
        names: &'a Option<HashMap<String, String>>,
        values: &'a Option<HashMap<String, AttributeValue>>,
    ) -> TrackedExpressionAttributes<'a> {
        TrackedExpressionAttributes::new(names, values)
    }

    #[test]
    fn test_set_simple() {
        let expr = parse("SET label = :val").unwrap();
        assert_eq!(expr.set_actions.len(), 1);
        assert!(expr.remove_actions.is_empty());
    }

    #[test]
    fn test_set_multiple() {
        let expr = parse("SET a = :v1, b = :v2").unwrap();
        assert_eq!(expr.set_actions.len(), 2);
    }

    #[test]
    fn test_set_arithmetic_plus() {
        let expr = parse("SET tally = tally + :inc").unwrap();
        let mut item = make_item(&[
            ("pk", AttributeValue::S("k".into())),
            ("tally", AttributeValue::N("10".into())),
        ]);
        let av = vals(&[(":inc", AttributeValue::N("5".into()))]);
        let no_names = None;
        let tracker = make_tracker(&no_names, &av);
        apply(&mut item, &expr, &tracker).unwrap();
        assert_eq!(item["tally"], AttributeValue::N("15".into()));
    }

    #[test]
    fn test_set_arithmetic_minus() {
        let expr = parse("SET price = price - :discount").unwrap();
        let mut item = make_item(&[
            ("pk", AttributeValue::S("k".into())),
            ("price", AttributeValue::N("100".into())),
        ]);
        let av = vals(&[(":discount", AttributeValue::N("25".into()))]);
        let no_names = None;
        let tracker = make_tracker(&no_names, &av);
        apply(&mut item, &expr, &tracker).unwrap();
        assert_eq!(item["price"], AttributeValue::N("75".into()));
    }

    #[test]
    fn test_set_if_not_exists() {
        let expr = parse("SET hits = if_not_exists(hits, :zero)").unwrap();
        let mut item = make_item(&[("pk", AttributeValue::S("k".into()))]);
        let av = vals(&[(":zero", AttributeValue::N("0".into()))]);
        let no_names = None;
        let tracker = make_tracker(&no_names, &av);
        apply(&mut item, &expr, &tracker).unwrap();
        assert_eq!(item["hits"], AttributeValue::N("0".into()));

        // Apply again: existing value should be preserved
        let tracker2 = make_tracker(&no_names, &av);
        apply(&mut item, &expr, &tracker2).unwrap();
        assert_eq!(item["hits"], AttributeValue::N("0".into()));
    }

    #[test]
    fn test_set_list_append() {
        let expr = parse("SET entries = list_append(entries, :new)").unwrap();
        let mut item = make_item(&[
            ("pk", AttributeValue::S("k".into())),
            (
                "entries",
                AttributeValue::L(vec![AttributeValue::S("a".into())]),
            ),
        ]);
        let av = vals(&[(
            ":new",
            AttributeValue::L(vec![AttributeValue::S("b".into())]),
        )]);
        let no_names = None;
        let tracker = make_tracker(&no_names, &av);
        apply(&mut item, &expr, &tracker).unwrap();
        if let AttributeValue::L(list) = &item["entries"] {
            assert_eq!(list.len(), 2);
        } else {
            panic!("Expected list");
        }
    }

    #[test]
    fn test_remove() {
        let expr = parse("REMOVE attr1, attr2").unwrap();
        let mut item = make_item(&[
            ("pk", AttributeValue::S("k".into())),
            ("attr1", AttributeValue::S("a".into())),
            ("attr2", AttributeValue::S("b".into())),
            ("attr3", AttributeValue::S("c".into())),
        ]);
        let no_names = None;
        let no_values = None;
        let tracker = make_tracker(&no_names, &no_values);
        apply(&mut item, &expr, &tracker).unwrap();
        assert!(!item.contains_key("attr1"));
        assert!(!item.contains_key("attr2"));
        assert!(item.contains_key("attr3"));
    }

    #[test]
    fn test_add_number() {
        let expr = parse("ADD tally :inc").unwrap();
        let mut item = make_item(&[
            ("pk", AttributeValue::S("k".into())),
            ("tally", AttributeValue::N("10".into())),
        ]);
        let av = vals(&[(":inc", AttributeValue::N("5".into()))]);
        let no_names = None;
        let tracker = make_tracker(&no_names, &av);
        apply(&mut item, &expr, &tracker).unwrap();
        assert_eq!(item["tally"], AttributeValue::N("15".into()));
    }

    #[test]
    fn test_add_number_create() {
        let expr = parse("ADD tally :val").unwrap();
        let mut item = make_item(&[("pk", AttributeValue::S("k".into()))]);
        let av = vals(&[(":val", AttributeValue::N("1".into()))]);
        let no_names = None;
        let tracker = make_tracker(&no_names, &av);
        apply(&mut item, &expr, &tracker).unwrap();
        assert_eq!(item["tally"], AttributeValue::N("1".into()));
    }

    #[test]
    fn test_add_string_set() {
        let expr = parse("ADD colors :new_colors").unwrap();
        let mut item = make_item(&[
            ("pk", AttributeValue::S("k".into())),
            (
                "colors",
                AttributeValue::SS(vec!["red".into(), "blue".into()]),
            ),
        ]);
        let av = vals(&[(
            ":new_colors",
            AttributeValue::SS(vec!["blue".into(), "green".into()]),
        )]);
        let no_names = None;
        let tracker = make_tracker(&no_names, &av);
        apply(&mut item, &expr, &tracker).unwrap();
        if let AttributeValue::SS(set) = &item["colors"] {
            assert_eq!(set.len(), 3); // red, blue, green (blue deduplicated)
            assert!(set.contains(&"green".to_string()));
        } else {
            panic!("Expected SS");
        }
    }

    #[test]
    fn test_delete_string_set() {
        let expr = parse("DELETE colors :remove").unwrap();
        let mut item = make_item(&[
            ("pk", AttributeValue::S("k".into())),
            (
                "colors",
                AttributeValue::SS(vec!["red".into(), "blue".into(), "green".into()]),
            ),
        ]);
        let av = vals(&[(
            ":remove",
            AttributeValue::SS(vec!["blue".into(), "green".into()]),
        )]);
        let no_names = None;
        let tracker = make_tracker(&no_names, &av);
        apply(&mut item, &expr, &tracker).unwrap();
        if let AttributeValue::SS(set) = &item["colors"] {
            assert_eq!(set, &vec!["red".to_string()]);
        } else {
            panic!("Expected SS");
        }
    }

    #[test]
    fn test_combined_set_remove() {
        let expr = parse("SET label = :name REMOVE old_attr").unwrap();
        assert_eq!(expr.set_actions.len(), 1);
        assert_eq!(expr.remove_actions.len(), 1);
    }

    #[test]
    fn test_duplicate_clause_error() {
        let result = parse("SET a = :v SET b = :w");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("only be used once"));
    }

    /// #35(a): a later SET reads the pre-update value of an earlier target.
    #[test]
    fn test_set_reads_pre_update_snapshot() {
        let expr = parse("SET a = :v, b = a").unwrap();
        let mut item = make_item(&[
            ("pk", AttributeValue::S("k".into())),
            ("a", AttributeValue::S("OLD".into())),
        ]);
        let av = vals(&[(":v", AttributeValue::S("NEW".into()))]);
        let no_names = None;
        let tracker = make_tracker(&no_names, &av);
        apply(&mut item, &expr, &tracker).unwrap();
        assert_eq!(item["a"], AttributeValue::S("NEW".into()));
        assert_eq!(item["b"], AttributeValue::S("OLD".into()));
    }

    /// #35(b): a parenthesised arithmetic group parses and evaluates.
    #[test]
    fn test_set_parenthesised_arithmetic() {
        let expr = parse("SET c = (c - :v)").unwrap();
        let mut item = make_item(&[
            ("pk", AttributeValue::S("k".into())),
            ("c", AttributeValue::N("10".into())),
        ]);
        let av = vals(&[(":v", AttributeValue::N("3".into()))]);
        let no_names = None;
        let tracker = make_tracker(&no_names, &av);
        apply(&mut item, &expr, &tracker).unwrap();
        assert_eq!(item["c"], AttributeValue::N("7".into()));
    }

    /// #35(b): high-precision arithmetic inside a group stays exact (BigDecimal path).
    #[test]
    fn test_set_parenthesised_arithmetic_bigdecimal() {
        let expr = parse("SET c = (c + :v)").unwrap();
        let mut item = make_item(&[
            ("pk", AttributeValue::S("k".into())),
            ("c", AttributeValue::N("100000000000000000000".into())),
        ]);
        let av = vals(&[(":v", AttributeValue::N("1".into()))]);
        let no_names = None;
        let tracker = make_tracker(&no_names, &av);
        apply(&mut item, &expr, &tracker).unwrap();
        assert_eq!(item["c"], AttributeValue::N("100000000000000000001".into()));
    }

    #[test]
    fn update_expression_syntax_error_includes_quoted_token_and_near_window() {
        // The conformance suite pins the AWS string for "INVALID SYNTAX HERE":
        //   Invalid UpdateExpression: Syntax error; token: "INVALID", near: "INVALID SYNTAX"
        let err = parse("INVALID SYNTAX HERE").unwrap_err();
        assert_eq!(
            err,
            r#"Invalid UpdateExpression: Syntax error; token: "INVALID", near: "INVALID SYNTAX""#
        );
    }
}
