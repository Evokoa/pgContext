#!/usr/bin/env python3
"""Make the generated pgContext SQL artifact pgvector-coexist safe.

cargo-pgrx emits unconditional ``CREATE TYPE vector/halfvec/sparsevec``
blocks, 23 ``CREATE CAST`` statements over those types, and three
``ALTER TYPE ... SET (TYPMOD_IN ...)`` statements. All of these collide
with (or, worse, silently mutate) the pgvector extension's own objects
when pgvector is already installed. This transform wraps exactly those
statements in ``DO`` blocks guarded by a pg_extension lookup so that:

- without pgvector: behavior is byte-for-byte identical to today (every
  guarded statement executes);
- with pgvector: the colliding objects are skipped and every other
  pgContext object binds to pgvector's identically-laid-out types by
  name resolution (opclasses are already spelled ``public.vector`` etc.).

Objects created via EXECUTE inside an extension script are still
extension members, so no ALTER EXTENSION bookkeeping is needed.

Used by BOTH the artifact regeneration flow and
``scripts/check-extension-sql-artifact.sh`` (which transforms its freshly
generated copy before diffing) — the two must never diverge. Packaging
paths that regenerate SQL (pgxn dist, release images) must run this
transform too; ``cargo pgrx install`` does not, so local coexist testing
must transform the installed ``pgcontext--*.sql`` afterwards.

Usage: transform-sql-artifact-coexist.py FILE   (in-place, idempotent)
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

MARKER = "$pgcontext_coexist$"
STMT_TAG = "$pgcontext_stmt$"
GUARD_TYPES = ("vector", "halfvec", "sparsevec")  # bitvec is ours alone

SHELL_TYPE_RE = re.compile(
    r"^CREATE TYPE (Vector|HalfVec|SparseVec);\s*$", re.MULTILINE
)
CAST_RE = re.compile(r"^CREATE CAST \(([^)]*)\)", re.IGNORECASE)
ALTER_RE = re.compile(
    r"^ALTER TYPE (vector|halfvec|sparsevec)\b", re.IGNORECASE
)
# Our btree ordering opclasses are schema-qualified (no name collision)
# but declare DEFAULT FOR TYPE on the shared type names; PostgreSQL
# allows only one default per (type, AM) and pgvector's vector_ops
# already holds it. In coexist mode pgvector's btree ordering semantics
# apply; ours are skipped. (The pgcontext_hnsw opclasses keep their
# DEFAULT: that AM is ours alone, so no competing default can exist.)
DEFAULT_ORDERING_OPCLASS_RE = re.compile(
    r"^CREATE OPERATOR CLASS pgcontext\.(vector|halfvec|sparsevec)_ops\b",
    re.IGNORECASE,
)


def split_statements(block: str) -> list[tuple[str, str]]:
    """Split block text into ('sql'|'other', chunk) pieces.

    A statement is lines accumulated until one ends with ';'. Comment or
    blank lines between statements are 'other' and pass through
    untouched. The artifact contains no embedded semicolons outside
    statement ends (verified against the checked-in artifact).
    """
    pieces: list[tuple[str, str]] = []
    pending: list[str] = []
    for line in block.splitlines(keepends=True):
        stripped = line.strip()
        in_statement = bool(pending)
        starts_sql = bool(
            re.match(r"(CREATE|ALTER|DROP|COMMENT|GRANT|SELECT|DO)\b", stripped)
        )
        if not in_statement and not starts_sql:
            pieces.append(("other", line))
            continue
        pending.append(line)
        if stripped.endswith(";"):
            pieces.append(("sql", "".join(pending)))
            pending = []
    if pending:  # unterminated tail — treat as passthrough, fail closed later
        pieces.append(("other", "".join(pending)))
    return pieces


def needs_guard(stmt: str) -> bool:
    first = stmt.lstrip().splitlines()[0]
    if ALTER_RE.match(first) or DEFAULT_ORDERING_OPCLASS_RE.match(first):
        return True
    cast = CAST_RE.match(first)
    if cast:
        inner = cast.group(1).lower()
        return any(re.search(rf"\b{t}\b", inner) for t in GUARD_TYPES)
    return False


def wrap(statements: list[str]) -> str:
    for stmt in statements:
        if MARKER in stmt or STMT_TAG in stmt:
            raise SystemExit(
                "refusing to wrap: dollar-quote tag already present in input"
            )
    body = "".join(
        f"    EXECUTE {STMT_TAG}\n{stmt.rstrip()}\n{STMT_TAG};\n"
        for stmt in statements
    )
    return (
        f"DO {MARKER}\nBEGIN\n"
        "  IF NOT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'vector') THEN\n"
        f"{body}"
        "  END IF;\n"
        f"END\n{MARKER};\n"
    )


def transform_block(block: str) -> str:
    """Transform one connected-objects block (or the prefix chunk)."""
    is_guarded_type_block = bool(SHELL_TYPE_RE.search(block))
    pieces = split_statements(block)
    out: list[str] = []
    run: list[str] = []  # contiguous statements to guard

    def flush() -> None:
        if run:
            out.append(wrap(run))
            run.clear()

    for kind, chunk in pieces:
        if kind == "sql" and (is_guarded_type_block or needs_guard(chunk)):
            run.append(chunk)
        elif kind == "other" and run and chunk.strip() == "":
            # blank line inside a guarded run: keep the run contiguous
            continue
        else:
            flush()
            out.append(chunk)
    flush()
    return "".join(out)


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    path = Path(sys.argv[1])
    text = path.read_text()
    if MARKER in text:
        print(f"already transformed: {path}")
        return

    begin = "/* <begin connected objects> */"
    parts = text.split(begin)
    prefix, blocks = parts[0], parts[1:]
    out = [transform_block(prefix)]
    for block in blocks:
        out.append(begin)
        out.append(transform_block(block))
    result = "".join(out)

    guarded = result.count(f"DO {MARKER}")
    shell_types = len(SHELL_TYPE_RE.findall(text))
    if shell_types != 3:
        raise SystemExit(
            f"expected 3 colliding shell types in artifact, found {shell_types}; "
            "artifact shape changed — update this transform deliberately"
        )
    if guarded == 0:
        raise SystemExit("transform produced no guards; refusing to write")
    path.write_text(result)
    print(f"transformed {path}: {guarded} guarded DO blocks")


if __name__ == "__main__":
    main()
