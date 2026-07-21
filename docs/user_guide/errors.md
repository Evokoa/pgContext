# Error Categories And SQLSTATEs

pgContext uses stable error categories for SQL-visible failures. Error messages
may become more specific, but clients should branch on SQLSTATE and documented
category instead of parsing message text.

SQLSTATE compatibility is part of the stable SQL API. Changing the SQLSTATE for
the same documented failure class is a breaking change unless the old path was
experimental or internal.

| Category | SQLSTATE | Meaning |
|---|---:|---|
| `InvalidVector` | `22P02` | Malformed vector input |
| `DimensionMismatch` | `22023` | Valid input with incompatible dimensions |
| `InvalidFilter` | `22023` | Malformed or semantically invalid filter input |
| `UnsafePredicate` | `22023` | Rejected predicate rendering or unsafe path input |
| `UnknownCollection` | `42704` | Registered collection does not exist |
| `UnknownVector` | `42704` | Registered vector name does not exist |
| `UnknownPayloadField` | `42703` | Registered payload source column does not exist |
| `UnknownTable` | `42P01` | Referenced source table does not exist |
| `WrongColumnType` | `42804` | Referenced source column has an incompatible SQL type |
| `DuplicateRegistration` | `42710` | Collection, vector, payload, or model registration already exists |
| `AclDenied` | `42501` | PostgreSQL ACL, RLS, or ownership check denied access |
| `IndexNotReady` | `55000` | Index or artifact cannot currently serve queries |
| `IndexCorrupt` | `XX001` | Index or artifact validation found corruption |
| `RecallBudgetExceeded` | `54000` | Search, recall-check, or candidate-recheck work exceeded a configured resource budget |
| `UnsupportedMetric` | `0A000` | Requested metric is not supported for this vector/index kind |
| `UnsupportedPostgresVersion` | `0A000` | PostgreSQL version is outside the supported matrix |
| `Internal` | `XX000` | Unexpected internal invariant failure |

## Dense HNSW Boundary Errors

Dense HNSW preserves the vector categories above and adds PostgreSQL catalog
validation where appropriate:

| Failure | SQLSTATE | Message category |
|---|---:|---|
| Non-finite text or values | `22P02` | `InvalidVector` |
| Dimension mismatch or invalid HNSW settings | `22023` | `DimensionMismatch` / invalid parameter |
| Forbidden vector representation cast | `42846` | Cannot coerce |
| Unsupported or internally inconsistent operator class | `42P17` | Invalid object definition |
| Stored metapage metric/configuration does not match the index | `XX001` | `IndexCorrupt` |

A `NULL` indexed vector is not an error: the access method stores no entry for
that row. Cosine indexes reject zero-magnitude vectors with `22023` during
build or insert because the selected metric is undefined for that value.
