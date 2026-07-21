#!/usr/bin/env python3
"""Unit tests for the macOS live-PostgreSQL pg_test runner."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
RUNNER_PATH = REPO_ROOT / "scripts" / "run_pgrx_tests_in_server.py"
SPEC = importlib.util.spec_from_file_location("run_pgrx_tests_in_server", RUNNER_PATH)
assert SPEC is not None and SPEC.loader is not None
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class ParsePgTestsTest(unittest.TestCase):
    def test_parses_plain_and_expected_error_tests(self) -> None:
        source = r'''
#[pg_test]
fn succeeds() {}

#[pg_test]
#[should_panic(expected = "permission denied for table \"private\"")]
fn reports_expected_error() {}

#[pg_test]
#[should_panic(
    expected = "first line\nsecond line",
)]
fn reports_multiline_error() {}
'''
        with tempfile.TemporaryDirectory() as temporary_directory:
            path = Path(temporary_directory) / "sample.rs"
            path.write_text(source, encoding="utf-8")

            tests = RUNNER.parse_pg_test_file(path, Path(temporary_directory))

        self.assertEqual(
            [(test.name, test.expected_error) for test in tests],
            [
                ("succeeds", None),
                ("reports_expected_error", 'permission denied for table "private"'),
                ("reports_multiline_error", "first line\nsecond line"),
            ],
        )
        self.assertEqual(tests[0].source_path, "sample.rs")
        self.assertEqual(tests[0].source_line, 2)

    def test_resolves_generated_wrapper_name_from_installed_sql(self) -> None:
        source = "#[pg_test]\nfn a_test_name_that_postgresql_must_shorten() {}\n"
        installed_sql = """\
/* <begin connected objects> */
-- sample.rs:1
-- pgcontext::tests::t7_a_test_name_that_postgresql_must_shorten
CREATE  FUNCTION tests."t7_a_test_name_that_postgresql_must_shorten"() RETURNS void
STRICT
LANGUAGE c /* Rust */;
/* </end connected objects> */
"""
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            source_path = root / "sample.rs"
            sql_path = root / "pgcontext--0.1.0.sql"
            source_path.write_text(source, encoding="utf-8")
            sql_path.write_text(installed_sql, encoding="utf-8")
            tests = RUNNER.parse_pg_test_file(source_path, root)

            resolved = RUNNER.resolve_installed_test_names(tests, sql_path)

        self.assertEqual(
            resolved[0].function_name,
            "t7_a_test_name_that_postgresql_must_shorten",
        )

    def test_rejects_should_panic_without_an_expected_message(self) -> None:
        source = "#[pg_test]\n#[should_panic]\nfn ambiguous_error() {}\n"
        with tempfile.TemporaryDirectory() as temporary_directory:
            path = Path(temporary_directory) / "sample.rs"
            path.write_text(source, encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "expected message"):
                RUNNER.parse_pg_test_file(path, Path(temporary_directory))

    def test_discovery_rejects_duplicate_function_names(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            repo_root = Path(temporary_directory)
            pg_tests = repo_root / "crates" / "context-pg" / "src" / "pg_tests"
            pg_tests.mkdir(parents=True)
            (pg_tests / "one.rs").write_text("#[pg_test]\nfn duplicate() {}\n")
            (pg_tests / "two.rs").write_text("#[pg_test]\nfn duplicate() {}\n")

            with self.assertRaisesRegex(ValueError, "duplicate pg_test function"):
                RUNNER.discover_pg_tests(repo_root)


class RenderSqlTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tests = [
            RUNNER.PgTest("works", None, "tests.rs", 10, "works"),
            RUNNER.PgTest(
                "fails_as_expected",
                "expected failure",
                "tests.rs",
                20,
                "t9_fails_as_expected",
            ),
        ]

    def test_full_suite_setup_validates_the_installed_catalog(self) -> None:
        sql = RUNNER.render_setup_sql(self.tests, require_complete_catalog=True)

        self.assertIn("CREATE EXTENSION pgcontext", sql)
        self.assertIn("t9_fails_as_expected", sql)
        self.assertNotIn("pg_catalog.starts_with", sql)
        self.assertIn("full source/catalog pg_test count mismatch", sql)

    def test_single_test_sql_calls_the_wrapper_directly_in_a_transaction(self) -> None:
        sql = RUNNER.render_single_test_sql(self.tests[1])

        self.assertIn("BEGIN;", sql)
        self.assertIn("ROLLBACK;", sql)
        self.assertIn('tests."t9_fails_as_expected"()', sql)
        self.assertNotIn("EXCEPTION WHEN", sql)

    def test_expected_error_matches_the_client_error_as_a_substring(self) -> None:
        test = self.tests[1]

        self.assertIsNone(
            RUNNER.test_result_failure(
                test,
                returncode=3,
                output="ERROR: expected failure: point 92",
            )
        )
        self.assertIn(
            "unexpected error",
            RUNNER.test_result_failure(
                test,
                returncode=3,
                output="ERROR: a different failure",
            ),
        )
        self.assertIn(
            "succeeded",
            RUNNER.test_result_failure(test, returncode=0, output=""),
        )

    def test_filtered_sql_does_not_require_the_full_catalog_count(self) -> None:
        sql = RUNNER.render_setup_sql(self.tests[:1], require_complete_catalog=False)

        self.assertNotIn("full source/catalog pg_test count mismatch", sql)
        self.assertIn("pg_test source/catalog mapping failed", sql)


if __name__ == "__main__":
    unittest.main()
