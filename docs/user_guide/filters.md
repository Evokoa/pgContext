# Filters

pgContext accepts Qdrant-style JSON filters and converts them into a typed
AST before any SQL planning happens. The typed renderer builds SQL
predicate plans for registered PostgreSQL columns and JSONB paths, then
binds predicate values through SPI parameters during search, count, facet,
and payload mutation execution. Malformed filters are rejected before they
reach SQL rendering.

Boolean filters use `must`, `should`, and `must_not` arrays:

```json
{
  "must": [
    { "key": "tenant_id", "match": "acme" },
    { "key": "price", "range": { "gte": 10, "lt": 20 } }
  ],
  "should": [
    { "key": "metadata.topic", "match": { "value": "billing" } }
  ],
  "must_not": [
    { "key": "archived", "match": true }
  ]
}
```

The parser currently accepts scalar `match` values, `match.value`,
`match.any`, `match.except`, `range` bounds (`gt`, `gte`, `lt`, `lte`),
`is_null`, and `is_empty`.

Unknown object fields are rejected. Empty filters are rejected. Filter depth,
condition count, field-key length, and dotted path depth are bounded by
pgContext policy defaults before later planning phases resolve fields or render
SQL predicates.

Before a filter can be rendered as SQL, every field key must resolve against
registered filterable fields. Ordinary column fields resolve to validated SQL
column identifiers. JSONB fields resolve to a validated JSONB column identifier
and structural path segments. Unknown keys are rejected instead of being treated
as raw SQL identifiers or JSON path text.

Rendered predicate plans contain SQL text with positional placeholders plus an
ordered parameter list. Predicate values and JSONB path segments are represented
as parameters instead of being interpolated into SQL text. Each parameter also
records its SQL type intent, so the PostgreSQL adapter can bind JSONB paths as
`text[]`, JSONB comparison values as `jsonb`, and ordinary column values through
the surrounding SQL type context.

Use `pgcontext.register_jsonb_path` to expose a JSONB path as a filter and facet
field. Missing path values behave like SQL `NULL` for facets and are omitted
from counts.

Registered filter fields also define the payload mutation surface. Qdrant-style
`set_payload`, `delete_payload`, and `clear_payload` can update only registered
ordinary columns and JSONB paths, and they require source-table `UPDATE`
privilege. This prevents arbitrary payload keys from silently mutating
unregistered source-table columns.

## Field Semantics

Ordinary columns and JSONB paths deliberately follow PostgreSQL null and type
rules instead of inventing a separate document-store truth table.

- Missing ordinary columns cannot be referenced. Registering a missing column or
  filtering on an unregistered field fails with SQLSTATE `42703` or `22023`.
- SQL `NULL` ordinary-column values do not match scalar equality, `match.any`,
  `match.except`, or range predicates. Use `is_null` when null membership is the
  intended predicate.
- Missing JSONB paths are treated like SQL `NULL`: they do not match scalar or
  range predicates, and facets omit them from counts.
- JSONB `null` at a registered path is also treated as null for filter and facet
  purposes.
- Empty arrays and empty JSON objects are values, not missing fields. Use
  `is_empty` for empty-container checks; scalar equality and range predicates do
  not treat them as strings.
- Wrong-type comparisons do not coerce silently. Numeric range predicates apply
  to JSON numbers and numeric-compatible ordinary columns. Strings that look
  numeric remain strings unless PostgreSQL casts the registered ordinary column
  through its declared SQL type.
- Mixed numeric/string comparisons are intentionally not normalized across
  representations. Register the field with the representation clients should
  query, and keep payload writers consistent.
- Unknown object fields inside the filter JSON are rejected during parsing.
  Unknown filter keys are rejected during field resolution, before SQL is
  rendered.
