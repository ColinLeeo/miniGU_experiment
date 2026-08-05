use minigu_common::value::ScalarValue;

use crate::procedures::gcard_query::error::{GCardError, GCardResult};
use crate::procedures::gcard_query::types::{ComparisonOp, PredicateDef};

#[derive(Clone)]
pub(crate) struct ResolvedPredicate {
    pub predicate_id: Option<u32>,
    pub prop_index: usize,
    pub op: ComparisonOp,
    pub value: ScalarValue,
    pub values: Vec<ScalarValue>,
}

impl ResolvedPredicate {
    pub fn new(prop_index: usize, predicate: &PredicateDef) -> Self {
        Self {
            predicate_id: predicate.predicate_id,
            prop_index,
            op: predicate.op,
            value: predicate.value.clone(),
            values: predicate.values.clone(),
        }
    }

    pub fn evaluate(&self, actual: &ScalarValue) -> GCardResult<bool> {
        evaluate_predicate(actual, &self.op, &self.value, &self.values)
    }
}

pub(crate) fn evaluate_predicate(
    actual: &ScalarValue,
    op: &ComparisonOp,
    expected: &ScalarValue,
    expected_values: &[ScalarValue],
) -> GCardResult<bool> {
    use ComparisonOp::*;

    match op {
        IsNull => return Ok(scalar_is_null(actual)),
        IsNotNull => return Ok(!scalar_is_null(actual)),
        _ => {}
    }

    // SQL WHERE keeps only TRUE. Comparisons, IN, LIKE, and NOT LIKE all
    // evaluate to UNKNOWN (and therefore do not pass) when the input is NULL.
    if scalar_is_null(actual) {
        return Ok(false);
    }

    match op {
        Eq => compare_scalar_equality(actual, expected, false),
        Ne => compare_scalar_equality(actual, expected, true),
        Gt => compare_ordered(actual, expected, |a, b| {
            partial_cmp_scalar(a, b).map(|ord| ord == std::cmp::Ordering::Greater)
        }),
        Ge => compare_ordered(actual, expected, |a, b| {
            partial_cmp_scalar(a, b)
                .map(|ord| ord == std::cmp::Ordering::Greater || ord == std::cmp::Ordering::Equal)
        }),
        Lt => compare_ordered(actual, expected, |a, b| {
            partial_cmp_scalar(a, b).map(|ord| ord == std::cmp::Ordering::Less)
        }),
        Le => compare_ordered(actual, expected, |a, b| {
            partial_cmp_scalar(a, b)
                .map(|ord| ord == std::cmp::Ordering::Less || ord == std::cmp::Ordering::Equal)
        }),
        In => {
            for candidate in expected_values {
                if scalar_is_null(candidate) {
                    continue;
                }
                if scalar_values_equal(actual, candidate) {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Like | NotLike => {
            let ScalarValue::String(Some(actual)) = actual else {
                return Err(GCardError::InvalidData(format!(
                    "LIKE requires a string input, got {:?}",
                    actual
                )));
            };
            let ScalarValue::String(Some(pattern)) = expected else {
                return Err(GCardError::InvalidData(format!(
                    "LIKE requires a non-null string pattern, got {:?}",
                    expected
                )));
            };
            let matched = sql_like_matches(actual, pattern);
            Ok(if matches!(op, NotLike) {
                !matched
            } else {
                matched
            })
        }
        IsNull | IsNotNull => unreachable!("handled before NULL propagation"),
    }
}

fn compare_scalar_equality(
    actual: &ScalarValue,
    expected: &ScalarValue,
    negate: bool,
) -> GCardResult<bool> {
    if scalar_is_null(expected) {
        return Ok(false);
    }
    let equal = scalar_values_equal(actual, expected);
    Ok(if negate { !equal } else { equal })
}

fn scalar_values_equal(left: &ScalarValue, right: &ScalarValue) -> bool {
    left == right
}

pub(crate) fn scalar_is_null(value: &ScalarValue) -> bool {
    use ScalarValue::*;
    matches!(
        value,
        Null | Boolean(None)
            | Int8(None)
            | Int16(None)
            | Int32(None)
            | Int64(None)
            | UInt8(None)
            | UInt16(None)
            | UInt32(None)
            | UInt64(None)
            | Float32(None)
            | Float64(None)
            | String(None)
            | Vector { value: None, .. }
            | Vertex(None)
            | Edge(None)
    )
}

fn compare_ordered<F>(value: &ScalarValue, expected: &ScalarValue, cmp: F) -> GCardResult<bool>
where
    F: FnOnce(&ScalarValue, &ScalarValue) -> Option<bool>,
{
    if scalar_is_null(expected) {
        return Ok(false);
    }
    cmp(value, expected).ok_or_else(|| {
        GCardError::InvalidData(format!(
            "Cannot compare values: {:?} and {:?}",
            value, expected
        ))
    })
}

fn partial_cmp_scalar(a: &ScalarValue, b: &ScalarValue) -> Option<std::cmp::Ordering> {
    use ScalarValue::*;

    match (a, b) {
        (Int8(Some(a_val)), Int8(Some(b_val))) => Some(a_val.cmp(b_val)),
        (Int16(Some(a_val)), Int16(Some(b_val))) => Some(a_val.cmp(b_val)),
        (Int32(Some(a_val)), Int32(Some(b_val))) => Some(a_val.cmp(b_val)),
        (Int64(Some(a_val)), Int64(Some(b_val))) => Some(a_val.cmp(b_val)),
        (UInt8(Some(a_val)), UInt8(Some(b_val))) => Some(a_val.cmp(b_val)),
        (UInt16(Some(a_val)), UInt16(Some(b_val))) => Some(a_val.cmp(b_val)),
        (UInt32(Some(a_val)), UInt32(Some(b_val))) => Some(a_val.cmp(b_val)),
        (UInt64(Some(a_val)), UInt64(Some(b_val))) => Some(a_val.cmp(b_val)),
        (Float32(Some(a_val)), Float32(Some(b_val))) => Some(a_val.cmp(b_val)),
        (Float64(Some(a_val)), Float64(Some(b_val))) => Some(a_val.cmp(b_val)),
        (String(Some(a_val)), String(Some(b_val))) => Some(a_val.cmp(b_val)),
        (Boolean(Some(a_val)), Boolean(Some(b_val))) => Some(a_val.cmp(b_val)),
        _ => match (to_f64_opt(a), to_f64_opt(b)) {
            (Some(a), Some(b)) => {
                use ordered_float::OrderedFloat;
                Some(OrderedFloat(a).cmp(&OrderedFloat(b)))
            }
            _ => None,
        },
    }
}

fn to_f64_opt(value: &ScalarValue) -> Option<f64> {
    use ScalarValue::*;
    match value {
        Int8(Some(v)) => Some(*v as f64),
        Int16(Some(v)) => Some(*v as f64),
        Int32(Some(v)) => Some(*v as f64),
        Int64(Some(v)) => Some(*v as f64),
        UInt8(Some(v)) => Some(*v as f64),
        UInt16(Some(v)) => Some(*v as f64),
        UInt32(Some(v)) => Some(*v as f64),
        UInt64(Some(v)) => Some(*v as f64),
        Float32(Some(v)) => Some(v.into_inner() as f64),
        Float64(Some(v)) => Some(v.into_inner()),
        _ => None,
    }
}

/// Match SQL LIKE without an ESCAPE clause. `%` matches zero or more Unicode
/// scalar values and `_` matches exactly one. All other characters are
/// literals, matching the JOB-M SQL contract.
pub(crate) fn sql_like_matches(actual: &str, pattern: &str) -> bool {
    let actual: Vec<char> = actual.chars().collect();
    let mut previous = vec![false; actual.len() + 1];
    previous[0] = true;

    for token in pattern.chars() {
        let mut current = vec![false; actual.len() + 1];
        if token == '%' {
            current[0] = previous[0];
        }
        for index in 1..=actual.len() {
            current[index] = match token {
                '%' => previous[index] || current[index - 1],
                '_' => previous[index - 1],
                literal => previous[index - 1] && actual[index - 1] == literal,
            };
        }
        previous = current;
    }

    previous[actual.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_like_matches_job_m_patterns() {
        assert!(sql_like_matches("(voice) (uncredited)", "%(voice)%"));
        assert!(sql_like_matches("Money Talks", "%Money%"));
        assert!(sql_like_matches("follow", "%follow%"));
        assert!(sql_like_matches("ab", "a_"));
        assert!(!sql_like_matches("a", "a_"));
        assert!(sql_like_matches("anything", "%"));
    }

    #[test]
    fn not_like_keeps_sql_null_semantics() {
        assert!(
            !evaluate_predicate(
                &ScalarValue::String(None),
                &ComparisonOp::NotLike,
                &ScalarValue::String(Some("%x%".to_string())),
                &[],
            )
            .unwrap()
        );
    }

    #[test]
    fn in_ignores_null_candidates() {
        assert!(
            evaluate_predicate(
                &ScalarValue::Int64(Some(7)),
                &ComparisonOp::In,
                &ScalarValue::Null,
                &[
                    ScalarValue::Null,
                    ScalarValue::Int64(Some(7)),
                    ScalarValue::Int64(Some(9)),
                ],
            )
            .unwrap()
        );
    }
}
