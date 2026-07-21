# Metric Semantics

This document is the authoritative contract for exact vector metrics. Rust core
types own validation and computation. SQL functions and operators translate
PostgreSQL values into those types and do not define separate formulas.

## Definitions and Ordering

For equal-length numeric vectors `a` and `b`:

| Metric | Definition | Core / SQL return type | Nearest-first order |
|---|---|---|---|
| L2 | `sqrt(sum((a[i] - b[i])^2))` | `f32` / `real` | Ascending |
| Inner product | `sum(a[i] * b[i])` | `f32` / `real` | Descending |
| Negative inner product | `-sum(a[i] * b[i])` | `f32` / `real` | Ascending |
| Cosine distance | `1 - dot(a, b) / (norm(a) * norm(b))` | `f32` / `real` | Ascending |
| L1 | `sum(abs(a[i] - b[i]))` | `f32` / `real` | Ascending |

Dense `vector`, `halfvec`, and `sparsevec` use these definitions. Half-vector
values are widened to `f32` before calculation. Sparse calculation treats every
omitted coordinate as zero. Exact top-k uses the stated nearest-first direction
and breaks equal-score ties by ascending point ID.

For equal-length bit vectors:

| Metric | Definition | Core / SQL return type | Nearest-first order |
|---|---|---|---|
| Hamming distance | Number of positions where `a[i] != b[i]` | `usize` / `integer` for `bitvec`; `double precision` for built-in `bit` | Ascending |
| Jaccard distance | `1 - count(a AND b) / count(a OR b)` | `f64` / `double precision` | Ascending |

When both bit vectors contain no set bits, their union is empty and Jaccard
distance is defined as `0`. A `bitvec` value itself must contain at least one
bit; the empty-union rule therefore covers nonempty all-zero operands.

## Validation and Errors

- Both operands must declare the same dimension. Core returns a dimension
  mismatch; SQL reports SQLSTATE `22023` (`invalid_parameter_value`).
- Numeric vector values must be finite. `NaN`, positive infinity, and negative
  infinity are rejected while constructing or parsing the vector; SQL reports
  SQLSTATE `22P02` (`invalid_text_representation`). Metrics never sanitize or
  replace invalid coordinates.
- Cosine distance is undefined when either operand has zero magnitude. This
  includes a sparse vector with no stored entries. Core returns an invalid-vector
  error and SQL reports SQLSTATE `22P02`.
- Exact SQL metric functions and their operators are strict: if either operand
  is SQL `NULL`, the result is `NULL` and the core kernel is not called.

## Representation Conversion

Conversions are explicit and checked according to this complete matrix:

| Source → target | Dense | Half | Sparse | Bit |
|---|---|---|---|---|
| Dense | Lossless | Checked lossy | Lossless | Forbidden |
| Half | Lossless | Lossless | Lossless | Forbidden |
| Sparse | Lossless | Checked lossy | Lossless | Forbidden |
| Bit | Forbidden | Forbidden | Forbidden | Lossless |

Checked lossy conversion to half precision must validate the target range and
finite result; it is not an unchecked cast. Numeric-to-bit and bit-to-numeric
casts are forbidden because binary quantization is an index-layer operation,
not a representation conversion.

The generated [Exact Metric and Operator Matrix](metric_operator_matrix.md)
maps every public exact metric to its core method, SQL helper/operator, score
direction, and current lifecycle.
