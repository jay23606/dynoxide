//! The order DynamoDB returns set members in.
//!
//! String and binary sets come back sorted by their bytes on every surface.
//! Number sets come back in one of two orders, depending on the operation:
//!
//! - ascending numeric order from GetItem, BatchGetItem, Query and the
//!   ReturnValues of PutItem, UpdateItem and DeleteItem;
//! - the iteration order of a Java `HashSet<BigDecimal>` built from that
//!   numeric order, from Scan, TransactGetItems, the PartiQL operations, the
//!   items in a transaction's cancellation reasons and stream records.
//!
//! Both orders were captured against DynamoDB in ten regions on 9 October 2026.
//! Three regions (eu-west-2, eu-central-1, ap-southeast-2) return Scan numerically
//! and two (eu-west-2, eu-north-1) do the same for TransactGetItems and the
//! cancellation reasons; the other regions use the hash order, so Dynoxide does.
//!
//! Sets are stored sorted (the `sort_*_set` functions run as items are normalised
//! for a write), and [`AttributeValue`]'s `Serialize` sorts as it writes, so a row
//! stored before sets were sorted still reads back in order on the wire (HTTP,
//! MCP and wasm). The typed Rust API hands back such a row's sets as stored.

use crate::types::{AttributeValue, Item, normalize_number_for_sort};
use serde::ser::{SerializeMap, SerializeSeq, Serializer};
use std::collections::HashMap;

/// Sort a string set's members by their bytes.
pub(crate) fn sort_string_set(set: &mut [String]) {
    set.sort_unstable();
}

/// Sort a binary set's members by their bytes.
pub(crate) fn sort_binary_set(set: &mut [Vec<u8>]) {
    set.sort_unstable();
}

/// Sort a number set's members by value.
pub(crate) fn sort_number_set(set: &mut [String]) {
    set.sort_by_cached_key(|n| normalize_number_for_sort(n));
}

/// The members of a string set in byte order, borrowing when already sorted.
pub(crate) fn sorted_strings(set: &[String]) -> Vec<&str> {
    let mut out: Vec<&str> = set.iter().map(String::as_str).collect();
    if !out.is_sorted() {
        out.sort_unstable();
    }
    out
}

/// The members of a binary set in byte order.
pub(crate) fn sorted_binaries(set: &[Vec<u8>]) -> Vec<&[u8]> {
    let mut out: Vec<&[u8]> = set.iter().map(Vec::as_slice).collect();
    if !out.is_sorted() {
        out.sort_unstable();
    }
    out
}

/// The members of a number set in ascending numeric order.
///
/// A set of fewer than two members, or one already in order (every set stored
/// since sets were sorted on write), comes back without a sort key being built
/// for each member; only an unsorted set pays for the keyed sort.
pub(crate) fn sorted_numbers(set: &[String]) -> Vec<&str> {
    let members: Vec<&str> = set.iter().map(String::as_str).collect();
    if members.len() < 2 || numbers_in_order(&members) {
        return members;
    }
    let mut keyed: Vec<(String, &str)> = members
        .into_iter()
        .map(|n| (normalize_number_for_sort(n), n))
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    keyed.into_iter().map(|(_, n)| n).collect()
}

/// Whether `members` are in ascending numeric order, compared without
/// allocating. False if any member doesn't read as a number, which sends the
/// set to the keyed sort.
fn numbers_in_order(members: &[&str]) -> bool {
    members.windows(2).all(|pair| {
        matches!(
            (Number::read(pair[0]), Number::read(pair[1])),
            (Some(a), Some(b)) if a.cmp_value(&b) != std::cmp::Ordering::Greater
        )
    })
}

/// A DynamoDB number read in place, for comparing by value without building
/// [`normalize_number_for_sort`]'s key.
struct Number<'a> {
    negative: bool,
    /// Digits before the point, then after it, as written.
    int: &'a [u8],
    frac: &'a [u8],
    /// Index into `int` then `frac` of the first non-zero digit; `None` for zero.
    lead: Option<usize>,
    /// The power of ten of the first non-zero digit.
    magnitude: i64,
}

impl<'a> Number<'a> {
    fn read(s: &'a str) -> Option<Self> {
        let s = s.trim().as_bytes();
        let (negative, s) = match s.split_first() {
            Some((b'-', rest)) => (true, rest),
            _ => (false, s),
        };
        let (mantissa, exponent) = match s.iter().position(|&c| c == b'e' || c == b'E') {
            Some(i) => (
                &s[..i],
                std::str::from_utf8(&s[i + 1..]).ok()?.parse::<i64>().ok()?,
            ),
            None => (s, 0),
        };
        let (int, frac) = match mantissa.iter().position(|&c| c == b'.') {
            Some(i) => (&mantissa[..i], &mantissa[i + 1..]),
            None => (mantissa, &mantissa[mantissa.len()..]),
        };
        if int.is_empty() && frac.is_empty() || !int.iter().chain(frac).all(u8::is_ascii_digit) {
            return None;
        }
        let lead = int.iter().chain(frac).position(|&d| d != b'0');
        let magnitude = match lead {
            Some(k) => (int.len() as i64 - k as i64 - 1).checked_add(exponent)?,
            None => 0,
        };
        Some(Self {
            negative,
            int,
            frac,
            lead,
            magnitude,
        })
    }

    /// -1, 0 or 1.
    fn sign(&self) -> i8 {
        match (self.lead, self.negative) {
            (None, _) => 0,
            (Some(_), true) => -1,
            (Some(_), false) => 1,
        }
    }

    /// The significant digits, from the first non-zero one.
    fn significant(&self) -> impl Iterator<Item = u8> + '_ {
        self.int
            .iter()
            .chain(self.frac)
            .copied()
            .skip(self.lead.unwrap_or(usize::MAX))
    }

    fn cmp_value(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let by_sign = self.sign().cmp(&other.sign());
        if by_sign != Ordering::Equal || self.sign() == 0 {
            return by_sign;
        }
        let by_size = self.magnitude.cmp(&other.magnitude).then_with(|| {
            let (mut a, mut b) = (self.significant(), other.significant());
            loop {
                match (a.next(), b.next()) {
                    (None, None) => return Ordering::Equal,
                    (Some(x), Some(y)) if x != y => return x.cmp(&y),
                    (Some(_), Some(_)) => {}
                    // A longer run is larger only if a non-zero digit is left.
                    (Some(x), None) => {
                        return if x != b'0' || a.any(|d| d != b'0') {
                            Ordering::Greater
                        } else {
                            Ordering::Equal
                        };
                    }
                    (None, Some(y)) => {
                        return if y != b'0' || b.any(|d| d != b'0') {
                            Ordering::Less
                        } else {
                            Ordering::Equal
                        };
                    }
                }
            }
        });
        if self.sign() < 0 {
            by_size.reverse()
        } else {
            by_size
        }
    }
}

/// The members of a number set in the order a Java `HashSet<BigDecimal>`
/// iterates them, after adding them one by one in ascending numeric order.
///
/// Each member is hashed as `BigDecimal.stripTrailingZeros()` would be, and the
/// table grows the way `java.util.HashMap` grows: from 16 buckets, doubling when
/// the size passes three quarters of the capacity, or when a bucket reaches nine
/// entries while the table is under 64 buckets. A bucket that long in a table of
/// 64 or more becomes a tree in Java and iterates in an order this does not
/// model; that takes nine members colliding in one bucket, which DynamoDB's
/// number range makes rare.
pub(crate) fn java_hash_order(set: &[String]) -> Vec<&str> {
    let numeric = sorted_numbers(set);
    let hashes: Vec<u32> = numeric
        .iter()
        .map(|n| spread(big_decimal_hash(n)))
        .collect();

    let mut capacity: usize = 16;
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); capacity];
    let mut size = 0usize;
    for (i, &h) in hashes.iter().enumerate() {
        let idx = (h as usize) & (capacity - 1);
        buckets[idx].push(i);
        let long_bucket = buckets[idx].len() >= 9;
        size += 1;
        if long_bucket && capacity < 64 {
            capacity = grow(&mut buckets, &hashes, capacity);
        }
        if size > capacity / 4 * 3 {
            capacity = grow(&mut buckets, &hashes, capacity);
        }
    }
    buckets.into_iter().flatten().map(|i| numeric[i]).collect()
}

/// Double the table, keeping each bucket's relative order, as `HashMap.resize`
/// does when it splits a bucket into its low and high halves.
fn grow(buckets: &mut Vec<Vec<usize>>, hashes: &[u32], capacity: usize) -> usize {
    let doubled = capacity * 2;
    let mut next: Vec<Vec<usize>> = vec![Vec::new(); doubled];
    for bucket in buckets.drain(..) {
        for i in bucket {
            next[(hashes[i] as usize) & (doubled - 1)].push(i);
        }
    }
    *buckets = next;
    doubled
}

/// `HashMap.hash`: fold the high half of the hash into the low half.
fn spread(h: i32) -> u32 {
    let h = h as u32;
    h ^ (h >> 16)
}

/// `BigDecimal.hashCode()` of the number after `stripTrailingZeros()`.
fn big_decimal_hash(number: &str) -> i32 {
    use bigdecimal::BigDecimal;
    use bigdecimal::num_bigint::Sign;
    use std::str::FromStr;

    let Ok(parsed) = BigDecimal::from_str(number.trim()) else {
        return 0;
    };
    let (unscaled, scale) = parsed.normalized().as_bigint_and_exponent();
    if unscaled.sign() == Sign::NoSign {
        // Java returns BigDecimal.ZERO, whose scale is 0.
        return 0;
    }
    let scale = scale as i32;
    if let Ok(compact) = i64::try_from(&unscaled)
        && compact != i64::MIN
    {
        let magnitude = compact.unsigned_abs();
        let high = (magnitude >> 32) as u32 as i32;
        let low = (magnitude & 0xFFFF_FFFF) as i64;
        let temp = ((high.wrapping_mul(31)) as i64).wrapping_add(low) as i32;
        let signed = if compact < 0 {
            temp.wrapping_neg()
        } else {
            temp
        };
        return 31i32.wrapping_mul(signed).wrapping_add(scale);
    }
    // BigInteger.hashCode(): the magnitude's 32-bit words, most significant
    // first, folded with 31, then multiplied by the signum.
    let (sign, mut words) = unscaled.to_u32_digits();
    words.reverse();
    let mut hash: i32 = 0;
    for word in words {
        hash = (31i64 * hash as i64 + word as i64) as i32;
    }
    let signum = if sign == Sign::Minus { -1 } else { 1 };
    31i32
        .wrapping_mul(hash.wrapping_mul(signum))
        .wrapping_add(scale)
}

/// An attribute value serialised with its number sets in Java hash order, at
/// any depth. String and binary sets keep their byte order.
struct HashOrdered<'a>(&'a AttributeValue);

impl serde::Serialize for HashOrdered<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            AttributeValue::NS(set) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("NS", &java_hash_order(set))?;
                map.end()
            }
            AttributeValue::L(list) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("L", &HashOrderedList(list))?;
                map.end()
            }
            AttributeValue::M(doc) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("M", &HashOrderedItem(doc))?;
                map.end()
            }
            other => other.serialize(serializer),
        }
    }
}

struct HashOrderedList<'a>(&'a [AttributeValue]);

impl serde::Serialize for HashOrderedList<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for value in self.0 {
            seq.serialize_element(&HashOrdered(value))?;
        }
        seq.end()
    }
}

struct HashOrderedItem<'a>(&'a HashMap<String, AttributeValue>);

impl serde::Serialize for HashOrderedItem<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, value) in self.0 {
            map.serialize_entry(name, &HashOrdered(value))?;
        }
        map.end()
    }
}

/// `serialize_with` for an optional item whose number sets DynamoDB returns in
/// hash order.
pub(crate) fn item_in_hash_order<S: Serializer>(
    item: &Option<Item>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match item {
        Some(item) => serializer.serialize_some(&HashOrderedItem(item)),
        None => serializer.serialize_none(),
    }
}

/// `serialize_with` for an optional list of items whose number sets DynamoDB
/// returns in hash order.
pub(crate) fn items_in_hash_order<S: Serializer>(
    items: &Option<Vec<Item>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    struct Items<'a>(&'a [Item]);
    impl serde::Serialize for Items<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
            for item in self.0 {
                seq.serialize_element(&HashOrderedItem(item))?;
            }
            seq.end()
        }
    }
    match items {
        Some(items) => serializer.serialize_some(&Items(items)),
        None => serializer.serialize_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    // Captured from a PartiQL SELECT in eu-west-2 and us-east-1, 9 October 2026,
    // for a set written as 100, -1, 10, 0.5, 9, 5, -10, -0.5, 0, 1e3, 25, 2.50,
    // -1e-5, 1E10, 38 nines, -1E-130 and 1E-130.
    #[test]
    fn number_sets_iterate_like_a_java_hash_set_of_big_decimals() {
        let tiny = format!("0.{}1", "0".repeat(129));
        let negative_tiny = format!("-{tiny}");
        let nines = "9".repeat(38);
        let set = vec![
            "100".to_string(),
            "-1".to_string(),
            "10".to_string(),
            "0.5".to_string(),
            "9".to_string(),
            "5".to_string(),
            "-10".to_string(),
            "-0.5".to_string(),
            "0".to_string(),
            "1000".to_string(),
            "25".to_string(),
            "2.5".to_string(),
            "-0.00001".to_string(),
            "10000000000".to_string(),
            nines.clone(),
            negative_tiny.clone(),
            tiny.clone(),
        ];
        let expected = vec![
            "0",
            tiny.as_str(),
            negative_tiny.as_str(),
            "25",
            nines.as_str(),
            "2.5",
            "10000000000",
            "9",
            "-0.5",
            "-0.00001",
            "5",
            "0.5",
            "1000",
            "100",
            "-1",
            "10",
            "-10",
        ];
        assert_eq!(java_hash_order(&set), expected);
    }

    // Twelve members stay in a 16-bucket table, which is what separates 9 from
    // 25 (same bucket, so numeric order) from a table sized for the collection.
    #[test]
    fn a_twelve_member_set_stays_in_sixteen_buckets() {
        let set = strings(&[
            "100", "-1", "10", "0.5", "9", "5", "-10", "-0.5", "0", "1000", "25", "2.5",
        ]);
        assert_eq!(
            java_hash_order(&set),
            [
                "0", "9", "25", "2.5", "-0.5", "5", "0.5", "1000", "100", "-1", "10", "-10"
            ]
        );
    }

    #[test]
    fn small_sets_reverse_or_keep_numeric_order_by_bucket() {
        assert_eq!(java_hash_order(&strings(&["2", "1", "3"])), ["3", "2", "1"]);
        assert_eq!(
            java_hash_order(&strings(&["20", "3", "100"])),
            ["3", "20", "100"]
        );
    }

    #[test]
    fn stored_order_sorts_numbers_by_value_and_strings_and_binaries_by_bytes() {
        let mut ns = strings(&["100", "-1", "10", "0.5", "-0.5", "0"]);
        sort_number_set(&mut ns);
        assert_eq!(ns, strings(&["-1", "-0.5", "0", "0.5", "10", "100"]));

        let mut ss = strings(&["b", "a", "B", "é", "ｚ", "😀", " "]);
        sort_string_set(&mut ss);
        assert_eq!(ss, strings(&[" ", "B", "a", "b", "é", "ｚ", "😀"]));

        let mut bs = vec![vec![0xff], vec![1, 0], vec![0x80], vec![1], vec![0]];
        sort_binary_set(&mut bs);
        assert_eq!(
            bs,
            vec![vec![0], vec![1], vec![1, 0], vec![0x80], vec![0xff]]
        );
    }

    proptest::proptest! {
        // The in-place comparison agrees with the sort key it stands in for,
        // across signs, leading and trailing zeros, points and exponents.
        #[test]
        fn reading_a_number_in_place_orders_it_as_its_sort_key_does(
            a_neg in proptest::bool::ANY, a_int in "[0-9]{0,6}", a_frac in proptest::option::of("[0-9]{0,6}"),
            a_exp in proptest::option::of(-12i64..12),
            b_neg in proptest::bool::ANY, b_int in "[0-9]{0,6}", b_frac in proptest::option::of("[0-9]{0,6}"),
            b_exp in proptest::option::of(-12i64..12),
        ) {
            let write = |neg: bool, int: &str, frac: &Option<String>, exp: Option<i64>| {
                let mut s = String::new();
                if neg { s.push('-'); }
                s.push_str(if int.is_empty() && frac.as_deref().unwrap_or("").is_empty() { "0" } else { int });
                if let Some(f) = frac { s.push('.'); s.push_str(f); }
                if let Some(e) = exp { s.push_str(&format!("e{e}")); }
                s
            };
            let a = write(a_neg, &a_int, &a_frac, a_exp);
            let b = write(b_neg, &b_int, &b_frac, b_exp);
            let (x, y) = (Number::read(&a).unwrap(), Number::read(&b).unwrap());
            proptest::prop_assert_eq!(
                x.cmp_value(&y),
                normalize_number_for_sort(&a).cmp(&normalize_number_for_sort(&b)),
                "{} vs {}", a, b
            );
        }
    }

    #[test]
    fn a_sorted_or_tiny_number_set_comes_back_as_it_is() {
        assert_eq!(sorted_numbers(&strings(&["7"])), vec!["7"]);
        assert_eq!(
            sorted_numbers(&strings(&["-1e3", "-2", "0", "0.5", "1", "1e3"])),
            vec!["-1e3", "-2", "0", "0.5", "1", "1e3"]
        );
        assert_eq!(
            sorted_numbers(&strings(&["10", "9", "1e1", "-0.5"])),
            vec!["-0.5", "9", "10", "1e1"]
        );
    }

    /// Every response field that carries an item, and the order its number
    /// sets come back in. A new response type that carries an item belongs
    /// here, with the order DynamoDB uses for it, so it can't silently get the
    /// other one.
    #[test]
    fn every_response_item_field_orders_number_sets_as_dynamodb_does() {
        use crate::actions::*;
        use serde_json::{Value, to_value};

        let members = strings(&["1", "2", "3"]);
        let numeric: Vec<&str> = vec!["1", "2", "3"];
        let hash: Vec<&str> = java_hash_order(&members);
        assert_ne!(
            numeric, hash,
            "the two orders must differ for this to test anything"
        );
        let item = || -> Item { [("ns".to_string(), AttributeValue::NS(members.clone()))].into() };
        let ns = |v: Value, pointer: &str| -> Vec<String> {
            v.pointer(pointer)
                .and_then(|ns| ns.as_array())
                .unwrap_or_else(|| panic!("no number set at {pointer} in {v}"))
                .iter()
                .map(|n| n.as_str().unwrap().to_string())
                .collect()
        };

        let cases: Vec<(&str, Value, &str, &Vec<&str>)> = vec![
            (
                "Scan",
                to_value(scan::ScanResponse {
                    items: Some(vec![item()]),
                    ..Default::default()
                })
                .unwrap(),
                "/Items/0/ns/NS",
                &hash,
            ),
            (
                "ExecuteStatement",
                to_value(execute_statement::ExecuteStatementResponse {
                    items: Some(vec![item()]),
                    ..Default::default()
                })
                .unwrap(),
                "/Items/0/ns/NS",
                &hash,
            ),
            (
                "BatchExecuteStatement",
                to_value(batch_execute_statement::BatchStatementResponse {
                    item: Some(item()),
                    ..Default::default()
                })
                .unwrap(),
                "/Item/ns/NS",
                &hash,
            ),
            (
                "TransactGetItems",
                to_value(transact_get_items::TransactGetResponse { item: Some(item()) }).unwrap(),
                "/Item/ns/NS",
                &hash,
            ),
            (
                "ExecuteTransaction",
                to_value(execute_transaction::ItemResponse { item: Some(item()) }).unwrap(),
                "/Item/ns/NS",
                &hash,
            ),
            (
                "stream NewImage",
                to_value(get_records::StreamRecord {
                    new_image: Some(item()),
                    ..Default::default()
                })
                .unwrap(),
                "/NewImage/ns/NS",
                &hash,
            ),
            (
                "stream OldImage",
                to_value(get_records::StreamRecord {
                    old_image: Some(item()),
                    ..Default::default()
                })
                .unwrap(),
                "/OldImage/ns/NS",
                &hash,
            ),
            (
                "cancellation reason",
                to_value(crate::errors::CancellationReason {
                    item: Some(item()),
                    ..Default::default()
                })
                .unwrap(),
                "/Item/ns/NS",
                &hash,
            ),
            (
                "GetItem",
                to_value(get_item::GetItemResponse {
                    item: Some(item()),
                    ..Default::default()
                })
                .unwrap(),
                "/Item/ns/NS",
                &numeric,
            ),
            (
                "Query",
                to_value(query::QueryResponse {
                    items: Some(vec![item()]),
                    ..Default::default()
                })
                .unwrap(),
                "/Items/0/ns/NS",
                &numeric,
            ),
            (
                "PutItem ReturnValues",
                to_value(put_item::PutItemResponse {
                    attributes: Some(item()),
                    ..Default::default()
                })
                .unwrap(),
                "/Attributes/ns/NS",
                &numeric,
            ),
            (
                "UpdateItem ReturnValues",
                to_value(update_item::UpdateItemResponse {
                    attributes: Some(item()),
                    ..Default::default()
                })
                .unwrap(),
                "/Attributes/ns/NS",
                &numeric,
            ),
            (
                "DeleteItem ReturnValues",
                to_value(delete_item::DeleteItemResponse {
                    attributes: Some(item()),
                    ..Default::default()
                })
                .unwrap(),
                "/Attributes/ns/NS",
                &numeric,
            ),
            (
                "BatchGetItem",
                to_value(batch_get_item::BatchGetItemResponse {
                    responses: [("tbl".to_string(), vec![item()])].into(),
                    ..Default::default()
                })
                .unwrap(),
                "/Responses/tbl/0/ns/NS",
                &numeric,
            ),
            // Not captured: the item on a ConditionalCheckFailed error and a
            // vector search hit follow AttributeValue's own (numeric) order.
            (
                "ConditionalCheckFailed item (inferred)",
                to_value(crate::errors::ErrorResponse {
                    error_type: String::new(),
                    message: String::new(),
                    item: Some(item()),
                })
                .unwrap(),
                "/Item/ns/NS",
                &numeric,
            ),
            (
                "SearchVectors hit (inferred)",
                to_value(search_vectors::SearchResult {
                    item: item(),
                    score: 0.0,
                })
                .unwrap(),
                "/Item/ns/NS",
                &numeric,
            ),
        ];
        for (surface, value, pointer, expected) in cases {
            assert_eq!(&ns(value, pointer), expected, "{surface}");
        }
    }
}
