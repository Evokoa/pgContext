#!/usr/bin/env python3
"""Run pgrx ``#[pg_test]`` wrappers inside a live PostgreSQL backend.

On macOS, the standalone Rust test executable produced by ``cargo pgrx test``
cannot resolve PostgreSQL server data symbols.  A test-enabled extension still
contains the generated SQL wrappers, so this runner discovers the Rust tests,
maps them to those wrappers, and preserves pgrx's transaction and expected-error
semantics while executing them through psql.
"""

from __future__ import annotations

import argparse
import ast
import re
import subprocess
import sys
from pathlib import Path
from typing import NamedTuple, Sequence


FUNCTION_RE = re.compile(
    r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\("
)
EXPECTED_RE = re.compile(r'expected\s*=\s*("(?:\\.|[^"\\])*")', re.DOTALL)
WRAPPER_RE = re.compile(
    r"^-- (?P<path>.+?\.rs):(?P<line>[0-9]+)\r?\n"
    r"-- [^\r\n]*::tests::(?P<name>[A-Za-z_][A-Za-z0-9_]*)\r?\n"
    r'CREATE\s+FUNCTION\s+tests\."(?P=name)"\(\)\s+RETURNS\s+void',
    re.MULTILINE,
)


class PgTest(NamedTuple):
    """A source-level pgrx test and its generated-wrapper expectations."""

    name: str
    expected_error: str | None
    source_path: str
    source_line: int
    function_name: str | None


def parse_pg_test_file(path: Path, repo_root: Path) -> list[PgTest]:
    """Parse source-level pg_tests without requiring Rust macro expansion."""

    lines = path.read_text(encoding="utf-8").splitlines()
    source_path = path.relative_to(repo_root).as_posix()
    tests: list[PgTest] = []
    line_index = 0

    while line_index < len(lines):
        if lines[line_index].strip() != "#[pg_test]":
            line_index += 1
            continue

        pg_test_line = line_index + 1
        following_lines: list[str] = []
        function_match: re.Match[str] | None = None
        scan_index = line_index + 1

        while scan_index < len(lines):
            candidate = lines[scan_index]
            if candidate.strip() == "#[pg_test]":
                raise ValueError(
                    f"{source_path}:{pg_test_line}: pg_test has no following function"
                )
            function_match = FUNCTION_RE.match(candidate)
            if function_match is not None:
                break
            following_lines.append(candidate)
            scan_index += 1

        if function_match is None:
            raise ValueError(
                f"{source_path}:{pg_test_line}: pg_test has no following function"
            )

        attribute_text = "\n".join(following_lines)
        expected_error: str | None = None
        if "should_panic" in attribute_text:
            expected_match = EXPECTED_RE.search(attribute_text)
            if expected_match is None:
                raise ValueError(
                    f"{source_path}:{pg_test_line}: should_panic requires an "
                    "expected message for live-backend execution"
                )
            try:
                decoded = ast.literal_eval(expected_match.group(1))
            except (SyntaxError, ValueError) as error:
                raise ValueError(
                    f"{source_path}:{pg_test_line}: invalid expected error string"
                ) from error
            if not isinstance(decoded, str):
                raise ValueError(
                    f"{source_path}:{pg_test_line}: expected error must be a string"
                )
            expected_error = decoded

        tests.append(
            PgTest(
                name=function_match.group(1),
                expected_error=expected_error,
                source_path=source_path,
                source_line=pg_test_line,
                function_name=None,
            )
        )
        line_index = scan_index + 1

    return tests


def resolve_installed_test_names(
    tests: Sequence[PgTest], extension_sql_path: Path
) -> list[PgTest]:
    """Resolve source tests to the exact SQL names emitted by pgrx."""

    try:
        installed_sql = extension_sql_path.read_text(encoding="utf-8")
    except OSError as error:
        raise ValueError(
            f"could not read installed extension SQL {extension_sql_path}: {error}"
        ) from error

    wrappers: dict[tuple[str, int], str] = {}
    for match in WRAPPER_RE.finditer(installed_sql):
        location = (match.group("path"), int(match.group("line")))
        previous = wrappers.get(location)
        if previous is not None:
            raise ValueError(
                f"installed extension SQL maps {location[0]}:{location[1]} more than once"
            )
        wrappers[location] = match.group("name")

    resolved: list[PgTest] = []
    source_locations = {(test.source_path, test.source_line) for test in tests}
    for test in tests:
        location = (test.source_path, test.source_line)
        function_name = wrappers.get(location)
        if function_name is None:
            raise ValueError(
                "installed extension SQL has no pg_test wrapper for "
                f"{test.source_path}:{test.source_line} ({test.name})"
            )
        resolved.append(test._replace(function_name=function_name))

    extra_locations = sorted(set(wrappers) - source_locations)
    if extra_locations:
        preview = ", ".join(f"{path}:{line}" for path, line in extra_locations[:5])
        raise ValueError(
            "installed extension SQL contains pg_test wrappers absent from the source tree: "
            f"{preview}"
        )
    return resolved


def discover_pg_tests(repo_root: Path) -> list[PgTest]:
    """Discover every pg_test in deterministic source order."""

    source_root = repo_root / "crates" / "context-pg" / "src" / "pg_tests"
    if not source_root.is_dir():
        raise ValueError(f"pg_test source directory does not exist: {source_root}")

    tests: list[PgTest] = []
    seen: dict[str, PgTest] = {}
    for path in sorted(source_root.rglob("*.rs")):
        for test in parse_pg_test_file(path, repo_root):
            previous = seen.get(test.name)
            if previous is not None:
                raise ValueError(
                    "duplicate pg_test function "
                    f"{test.name}: {previous.source_path}:{previous.source_line} and "
                    f"{test.source_path}:{test.source_line}"
                )
            seen[test.name] = test
            tests.append(test)

    if not tests:
        raise ValueError(f"no pg_tests found below {source_root}")
    return tests


def sql_literal(value: str | None) -> str:
    """Render a PostgreSQL string literal from trusted repository metadata."""

    if value is None:
        return "NULL"
    return "'" + value.replace("'", "''") + "'"


def render_setup_sql(
    tests: Sequence[PgTest], *, require_complete_catalog: bool
) -> str:
    """Render extension setup and source-to-catalog validation SQL."""

    if not tests:
        raise ValueError("at least one pg_test is required")
    unresolved = [test.name for test in tests if test.function_name is None]
    if unresolved:
        raise ValueError(
            "pg_tests must be resolved against the installed extension SQL: "
            + ", ".join(unresolved[:5])
        )

    rows = ",\n".join(
        "    ("
        f"{ordinal}, {sql_literal(test.name)}, {sql_literal(test.function_name)}, "
        f"{sql_literal(test.expected_error)}, {sql_literal(test.source_path)}, "
        f"{test.source_line}"
        ")"
        for ordinal, test in enumerate(tests, start=1)
    )
    full_catalog_guard = ""
    if require_complete_catalog:
        full_catalog_guard = """
    IF (SELECT count(*) FROM pgrx_source_tests)
       IS DISTINCT FROM (SELECT count(*) FROM pgrx_catalog_tests) THEN
        RAISE EXCEPTION
            'full source/catalog pg_test count mismatch: source=%, catalog=%',
            (SELECT count(*) FROM pgrx_source_tests),
            (SELECT count(*) FROM pgrx_catalog_tests);
    END IF;
"""

    return f"""\\set ON_ERROR_STOP on
CREATE EXTENSION pgcontext;

CREATE TEMP TABLE pgrx_source_tests (
    ordinal integer PRIMARY KEY,
    original_name text NOT NULL UNIQUE,
    function_name text NOT NULL UNIQUE,
    expected_error text,
    source_path text NOT NULL,
    source_line integer NOT NULL
);

INSERT INTO pgrx_source_tests
    (ordinal, original_name, function_name, expected_error, source_path, source_line)
VALUES
{rows};

CREATE TEMP TABLE pgrx_catalog_tests AS
SELECT procedure.oid AS function_oid,
       procedure.proname AS function_name
  FROM pg_catalog.pg_proc AS procedure
  JOIN pg_catalog.pg_namespace AS namespace
    ON namespace.oid = procedure.pronamespace
 WHERE namespace.nspname = 'tests'
   AND procedure.prokind = 'f'
   AND procedure.pronargs = 0
   AND procedure.prorettype = 'void'::pg_catalog.regtype;

CREATE TEMP TABLE pgrx_resolved_tests AS
SELECT source.ordinal,
       source.original_name,
       source.expected_error,
       source.source_path,
       source.source_line,
       catalog.function_oid,
       catalog.function_name
  FROM pgrx_source_tests AS source
  JOIN pgrx_catalog_tests AS catalog
    ON catalog.function_name = source.function_name;

DO $validation$
DECLARE
    mapping_failures text;
BEGIN
    SELECT pg_catalog.string_agg(
               pg_catalog.format('%s (%s matches)', original_name, match_count),
               ', ' ORDER BY original_name
           )
      INTO mapping_failures
      FROM (
          SELECT source.original_name,
                 count(resolved.function_oid) AS match_count
            FROM pgrx_source_tests AS source
            LEFT JOIN pgrx_resolved_tests AS resolved
              ON resolved.original_name = source.original_name
           GROUP BY source.original_name
          HAVING count(resolved.function_oid) <> 1
      ) AS failures;

    IF mapping_failures IS NOT NULL THEN
        RAISE EXCEPTION 'pg_test source/catalog mapping failed: %', mapping_failures;
    END IF;

    SELECT pg_catalog.string_agg(function_name, ', ' ORDER BY function_name)
      INTO mapping_failures
      FROM (
          SELECT function_name
            FROM pgrx_resolved_tests
           GROUP BY function_oid, function_name
          HAVING count(*) <> 1
      ) AS failures;

    IF mapping_failures IS NOT NULL THEN
        RAISE EXCEPTION 'generated pg_test wrapper mapped more than once: %',
            mapping_failures;
    END IF;
{full_catalog_guard}END
$validation$;
"""


def render_single_test_sql(test: PgTest) -> str:
    """Render one pg_test in a rollback transaction for a fresh backend."""

    if test.function_name is None:
        raise ValueError(
            f"pg_test {test.name} is not resolved against installed extension SQL"
        )
    if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", test.function_name) is None:
        raise ValueError(f"unsafe generated pg_test function name: {test.function_name}")

    return f"""\\set ON_ERROR_STOP on
\\set VERBOSITY terse
BEGIN;
SELECT tests."{test.function_name}"();
ROLLBACK;
"""


def test_result_failure(test: PgTest, *, returncode: int, output: str) -> str | None:
    """Return a pgrx-compatible failure description, or None for a pass."""

    if test.expected_error is None:
        if returncode == 0:
            return None
        return f"pg_test {test.name} failed"

    if returncode == 0:
        return f"pg_test {test.name} succeeded but expected error: {test.expected_error}"
    if test.expected_error not in output:
        return (
            f"pg_test {test.name} raised unexpected error; "
            f"expected message containing: {test.expected_error}"
        )
    return None


def build_argument_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo-root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
        help="pgContext repository root",
    )
    parser.add_argument("--psql", default="psql", help="PostgreSQL psql executable")
    parser.add_argument("--host", default="localhost")
    parser.add_argument("--port", default="28817")
    parser.add_argument("--database", required=True)
    parser.add_argument("--user")
    parser.add_argument(
        "--extension-sql",
        type=Path,
        required=True,
        help="test-enabled pgcontext--VERSION.sql produced by cargo pgrx install",
    )
    parser.add_argument(
        "--filter",
        help="run only tests whose Rust function name contains this substring",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_argument_parser().parse_args(argv)
    try:
        all_tests = resolve_installed_test_names(
            discover_pg_tests(args.repo_root.resolve()),
            args.extension_sql.resolve(),
        )
    except ValueError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2

    selected_tests = all_tests
    if args.filter:
        selected_tests = [test for test in all_tests if args.filter in test.name]
        if not selected_tests:
            print(f"error: no pg_tests matched filter {args.filter!r}", file=sys.stderr)
            return 2

    mode = "full suite" if args.filter is None else f"filter {args.filter!r}"
    print(
        f"running {len(selected_tests)} pg_tests in PostgreSQL ({mode})",
        file=sys.stderr,
        flush=True,
    )
    setup_sql = render_setup_sql(
        selected_tests,
        require_complete_catalog=args.filter is None,
    )
    command = [
        args.psql,
        "-X",
        "-q",
        "-v",
        "ON_ERROR_STOP=1",
        "-h",
        args.host,
        "-p",
        str(args.port),
        "-d",
        args.database,
    ]
    if args.user:
        command.extend(["-U", args.user])

    try:
        completed = subprocess.run(
            command,
            input=setup_sql,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
    except OSError as error:
        print(f"error: could not execute {args.psql}: {error}", file=sys.stderr)
        return 2
    if completed.returncode != 0:
        sys.stdout.write(completed.stdout)
        sys.stderr.write(completed.stderr)
        return completed.returncode

    total = len(selected_tests)
    for ordinal, test in enumerate(selected_tests, start=1):
        print(f"[{ordinal}/{total}] {test.name}", file=sys.stderr, flush=True)
        try:
            completed = subprocess.run(
                command,
                input=render_single_test_sql(test),
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
        except OSError as error:
            print(f"error: could not execute {args.psql}: {error}", file=sys.stderr)
            return 2
        output = completed.stdout + completed.stderr
        failure = test_result_failure(
            test,
            returncode=completed.returncode,
            output=output,
        )
        if failure is not None:
            print(f"error: {failure}", file=sys.stderr)
            sys.stdout.write(completed.stdout)
            sys.stderr.write(completed.stderr)
            return completed.returncode or 1

    print(f"pgrx_live_backend_complete: {total} tests")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
