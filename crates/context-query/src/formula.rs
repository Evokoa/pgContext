//! Bounded, side-effect-free query score formulas.

use crate::{QueryError, Result};

/// Maximum formula byte length preserved by the stable query-builder contract.
pub const MAX_FORMULA_BYTES: usize = 512;

/// Maximum arithmetic operations in one executable formula.
pub const MAX_FORMULA_OPERATIONS: usize = 128;

/// Validated bounded formula text.
///
/// Construction preserves the existing JSON-builder contract: any nonempty
/// bounded text is accepted. [`Formula::compile`] applies the stricter
/// executable grammar only when a plan is executed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Formula(String);

impl Formula {
    /// Creates bounded nonempty formula text.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the text is empty or exceeds
    /// [`MAX_FORMULA_BYTES`].
    pub fn new(formula: impl Into<String>) -> Result<Self> {
        let formula = formula.into();
        if formula.is_empty() || formula.len() > MAX_FORMULA_BYTES {
            return Err(invalid(format!(
                "query formula must be 1..={MAX_FORMULA_BYTES} bytes"
            )));
        }
        Ok(Self(formula))
    }

    /// Returns the validated formula text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the wrapper and returns the formula text.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }

    /// Compiles the formula into a bounded arithmetic expression.
    ///
    /// The grammar supports finite numeric literals, `$score` (or `score`),
    /// parentheses, unary `+`/`-`, and binary `+`, `-`, `*`, and `/`. It has no
    /// functions, field lookup, allocation during evaluation, or I/O.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for unsupported syntax, non-finite
    /// literals, trailing input, or an expression above
    /// [`MAX_FORMULA_OPERATIONS`].
    pub fn compile(&self) -> Result<CompiledFormula> {
        Parser::new(&self.0).parse()
    }
}

/// Compiled side-effect-free score expression.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledFormula {
    expression: Expression,
    operations: usize,
}

impl CompiledFormula {
    /// Returns the bounded arithmetic operation count.
    #[must_use]
    pub const fn operation_count(&self) -> usize {
        self.operations
    }

    /// Evaluates this expression for one finite branch score.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when `score` is non-finite, the
    /// caller's operation budget is too small, division by zero occurs, or the
    /// result is non-finite.
    pub fn evaluate(&self, score: f64, operation_budget: usize) -> Result<f64> {
        if !score.is_finite() {
            return Err(invalid("formula input score must be finite"));
        }
        if self.operations > operation_budget {
            return Err(QueryError::WorkBudgetExceeded {
                budget: "formula_operations",
                actual: self.operations,
                maximum: operation_budget,
            });
        }
        let value = self.expression.evaluate(score)?;
        if !value.is_finite() {
            return Err(invalid("formula result must be finite"));
        }
        Ok(value)
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Expression {
    Literal(f64),
    Score,
    Unary {
        negate: bool,
        operand: Box<Self>,
    },
    Binary {
        operator: BinaryOperator,
        left: Box<Self>,
        right: Box<Self>,
    },
}

impl Expression {
    fn evaluate(&self, score: f64) -> Result<f64> {
        match self {
            Self::Literal(value) => Ok(*value),
            Self::Score => Ok(score),
            Self::Unary { negate, operand } => {
                let value = operand.evaluate(score)?;
                Ok(if *negate { -value } else { value })
            }
            Self::Binary {
                operator,
                left,
                right,
            } => {
                let left = left.evaluate(score)?;
                let right = right.evaluate(score)?;
                match operator {
                    BinaryOperator::Add => Ok(left + right),
                    BinaryOperator::Subtract => Ok(left - right),
                    BinaryOperator::Multiply => Ok(left * right),
                    BinaryOperator::Divide if right == 0.0 => {
                        Err(invalid("formula division by zero"))
                    }
                    BinaryOperator::Divide => Ok(left / right),
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BinaryOperator {
    Add,
    Subtract,
    Multiply,
    Divide,
}

struct Parser<'a> {
    input: &'a [u8],
    position: usize,
    operations: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            position: 0,
            operations: 0,
        }
    }

    fn parse(mut self) -> Result<CompiledFormula> {
        let expression = self.parse_sum()?;
        self.skip_whitespace();
        if self.position != self.input.len() {
            return Err(invalid("formula contains unsupported trailing syntax"));
        }
        Ok(CompiledFormula {
            expression,
            operations: self.operations,
        })
    }

    fn parse_sum(&mut self) -> Result<Expression> {
        let mut expression = self.parse_product()?;
        loop {
            let operator = if self.consume(b'+') {
                Some(BinaryOperator::Add)
            } else if self.consume(b'-') {
                Some(BinaryOperator::Subtract)
            } else {
                None
            };
            let Some(operator) = operator else {
                return Ok(expression);
            };
            self.add_operation()?;
            let right = self.parse_product()?;
            expression = Expression::Binary {
                operator,
                left: Box::new(expression),
                right: Box::new(right),
            };
        }
    }

    fn parse_product(&mut self) -> Result<Expression> {
        let mut expression = self.parse_unary()?;
        loop {
            let operator = if self.consume(b'*') {
                Some(BinaryOperator::Multiply)
            } else if self.consume(b'/') {
                Some(BinaryOperator::Divide)
            } else {
                None
            };
            let Some(operator) = operator else {
                return Ok(expression);
            };
            self.add_operation()?;
            let right = self.parse_unary()?;
            expression = Expression::Binary {
                operator,
                left: Box::new(expression),
                right: Box::new(right),
            };
        }
    }

    fn parse_unary(&mut self) -> Result<Expression> {
        if self.consume(b'+') {
            self.add_operation()?;
            return Ok(Expression::Unary {
                negate: false,
                operand: Box::new(self.parse_unary()?),
            });
        }
        if self.consume(b'-') {
            self.add_operation()?;
            return Ok(Expression::Unary {
                negate: true,
                operand: Box::new(self.parse_unary()?),
            });
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expression> {
        self.skip_whitespace();
        if self.consume_raw(b'(') {
            let expression = self.parse_sum()?;
            if !self.consume(b')') {
                return Err(invalid("formula has an unmatched parenthesis"));
            }
            return Ok(expression);
        }
        if self.remaining().starts_with(b"$score") {
            self.position += b"$score".len();
            return Ok(Expression::Score);
        }
        if self.remaining().starts_with(b"score") {
            self.position += b"score".len();
            return Ok(Expression::Score);
        }
        self.parse_number()
    }

    fn parse_number(&mut self) -> Result<Expression> {
        self.skip_whitespace();
        let start = self.position;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.position += 1;
        }
        if self.peek() == Some(b'.') {
            self.position += 1;
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.position += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.position += 1;
            }
            let exponent_start = self.position;
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.position += 1;
            }
            if self.position == exponent_start {
                return Err(invalid("formula has an invalid numeric exponent"));
            }
        }
        if self.position == start {
            return Err(invalid("formula expected a number, score, or parenthesis"));
        }
        let literal = std::str::from_utf8(&self.input[start..self.position])
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite())
            .ok_or_else(|| invalid("formula numeric literal must be finite"))?;
        Ok(Expression::Literal(literal))
    }

    fn consume(&mut self, expected: u8) -> bool {
        self.skip_whitespace();
        self.consume_raw(expected)
    }

    fn consume_raw(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.position += 1;
        }
    }

    fn remaining(&self) -> &[u8] {
        &self.input[self.position..]
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }

    fn add_operation(&mut self) -> Result<()> {
        self.operations = self.operations.saturating_add(1);
        if self.operations > MAX_FORMULA_OPERATIONS {
            return Err(invalid(format!(
                "formula exceeds {MAX_FORMULA_OPERATIONS} arithmetic operations"
            )));
        }
        Ok(())
    }
}

fn invalid(reason: impl Into<String>) -> QueryError {
    QueryError::InvalidInput {
        field: "formula",
        reason: reason.into(),
    }
}
