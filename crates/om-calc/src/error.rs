//! Calculation error codes (spec `docs/re/calc-values.md` 7.1) and the
//! first-error-wins rule of an evaluation (7.2).

use std::fmt;

/// A FileMaker calculation error: the `Error::Code` an evaluation reports and
/// `EvaluationError()` returns (catalog `error-codes.csv`). Values that fail
/// become the `?` result; the error travels beside them. Codes are signed:
/// FileMaker reports -1 ("Unknown error") for some failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CalcError(i32);

impl CalcError {
    /// -1, "Unknown error": `JSONMakeArray` of empty text, the undocumented
    /// JSON value functions given extra arguments, `GetEmbeddingAsText` of
    /// text. It behaves like any other code: the first error wins and
    /// `EvaluationError` returns -1.
    pub const UNKNOWN: CalcError = CalcError(-1);
    /// 1, "User canceled action": the host stopped the evaluation
    /// ([`crate::eval::CalcHost::interrupted`]); it aborts the whole
    /// calculation. OpenMaker's own: FileMaker has no such stop inside a
    /// calculation.
    pub const CANCELED: CalcError = CalcError(1);
    /// 7: a `While` over its iteration limit; aborts the whole calculation.
    pub const OUT_OF_MEMORY: CalcError = CalcError(7);
    /// 14: `Sqrt(-1)`, `Ln(0)`, `10^405`, `Factorial(300)`.
    pub const OUT_OF_RANGE: CalcError = CalcError(14);
    /// 15: `1/0`, `Div(1;0)`, `0^-1`.
    pub const DIVIDE_BY_ZERO: CalcError = CalcError(15);
    /// 102: a field reference that does not resolve.
    pub const FIELD_MISSING: CalcError = CalcError(102);
    /// 106: an unknown name with no table context.
    pub const TABLE_MISSING: CalcError = CalcError(106);
    /// 500: a date or timestamp out of range or unparsable.
    pub const INVALID_DATE: CalcError = CalcError(500);
    /// 501: time text that cannot be parsed.
    pub const INVALID_TIME: CalcError = CalcError(501);
    /// 1202: too many parameters.
    pub const TOO_MANY_PARAMETERS: CalcError = CalcError(1202);
    /// 1204: number, text constant, field name or "(" expected.
    pub const OPERAND_EXPECTED: CalcError = CalcError(1204);
    /// 1205: comment not terminated.
    pub const COMMENT_NOT_TERMINATED: CalcError = CalcError(1205);
    /// 1206: text constant must end with a quotation mark.
    pub const TEXT_NOT_TERMINATED: CalcError = CalcError(1206);
    /// 1207: unbalanced parenthesis.
    pub const UNBALANCED_PARENTHESIS: CalcError = CalcError(1207);
    /// 1208: operator missing, function not found.
    pub const FUNCTION_NOT_FOUND: CalcError = CalcError(1208);
    /// 1211: list usage not allowed.
    pub const LIST_NOT_ALLOWED: CalcError = CalcError(1211);
    /// 1212: an operator is expected.
    pub const OPERATOR_EXPECTED: CalcError = CalcError(1212);
    /// 1214: a field is required.
    pub const FIELD_REQUIRED: CalcError = CalcError(1214);
    /// 1215: invalid `Get` parameter.
    pub const INVALID_GET_PARAMETER: CalcError = CalcError(1215);
    /// 1218: cannot evaluate the number (`5.`, `1e800`, `Asin(2)`).
    pub const CANNOT_EVALUATE_NUMBER: CalcError = CalcError(1218);
    /// 1225: not supported in this context.
    pub const NOT_SUPPORTED: CalcError = CalcError(1225);

    /// An error with any other positive code from `error-codes.csv`. Code 0
    /// means no error and is not a `CalcError`.
    pub const fn from_code(code: u16) -> Option<CalcError> {
        if code == 0 {
            None
        } else {
            Some(CalcError(code as i32))
        }
    }

    /// The numeric code, -1 for [`CalcError::UNKNOWN`].
    pub const fn code(self) -> i32 {
        self.0
    }
}

impl fmt::Display for CalcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "calculation error {}", self.0)
    }
}

impl std::error::Error for CalcError {}

/// The error state of one evaluation: the first error is kept and reported
/// even when the result does not depend on it (`Let(x=1/0;"ok")` returns `ok`
/// with error 15), later ones are ignored (spec 7.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FirstError(Option<CalcError>);

impl FirstError {
    /// Records `error` unless an earlier one is already held.
    pub fn record(&mut self, error: CalcError) {
        self.0.get_or_insert(error);
    }

    /// Records the error of `result`, if any, and passes its value through.
    pub fn check<T>(&mut self, result: Result<T, CalcError>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.record(error);
                None
            }
        }
    }

    /// The first error recorded.
    pub fn get(&self) -> Option<CalcError> {
        self.0
    }

    /// The code `EvaluationError()` reports: the first error's, or 0.
    pub fn code(&self) -> i32 {
        self.0.map_or(0, CalcError::code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_first_error() {
        let mut errors = FirstError::default();
        assert_eq!(errors.code(), 0);
        assert_eq!(errors.check(Ok::<_, CalcError>(1)), Some(1));
        assert_eq!(errors.check::<u8>(Err(CalcError::DIVIDE_BY_ZERO)), None);
        errors.record(CalcError::OUT_OF_RANGE);
        assert_eq!(errors.get(), Some(CalcError::DIVIDE_BY_ZERO));
        assert_eq!(errors.code(), 15);
    }

    #[test]
    fn codes_round_trip() {
        assert_eq!(CalcError::from_code(0), None);
        assert_eq!(
            CalcError::from_code(1218),
            Some(CalcError::CANNOT_EVALUATE_NUMBER)
        );
        assert_eq!(CalcError::INVALID_DATE.code(), 500);
        assert_eq!(CalcError::UNKNOWN.code(), -1);
        let mut errors = FirstError::default();
        errors.record(CalcError::UNKNOWN);
        errors.record(CalcError::DIVIDE_BY_ZERO);
        assert_eq!(errors.code(), -1);
    }
}
