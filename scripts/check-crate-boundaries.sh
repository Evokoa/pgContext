#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
guard_tmp="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-crate-boundaries.XXXXXX")"
trap 'rm -rf "${guard_tmp}"' EXIT

cargo metadata \
  --format-version 1 \
  --no-deps \
  --manifest-path "${REPO_ROOT}/Cargo.toml" \
  >"${guard_tmp}/metadata.json"

python3 - "${REPO_ROOT}" "${guard_tmp}/metadata.json" <<'PY'
import json
import re
import sys
from pathlib import Path

root = Path(sys.argv[1]).resolve()
metadata = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
packages = {package["name"]: package for package in metadata["packages"]}

pure_crates = (
    "context-core",
    "context-codec",
    "context-filter",
    "context-hybrid",
    "context-index",
    "context-storage",
    "context-query",
    "context-build",
    "pgcontext-worker",
)


def fail(message: str) -> None:
    print(message, file=sys.stderr)
    raise SystemExit(1)


for crate in (*pure_crates, "context-pg"):
    if crate not in packages:
        fail(f"missing workspace package: {crate}")


def dependency_records(crate: str):
    return packages[crate]["dependencies"]


def exact_dependencies(
    crate: str,
    required_normal: set[str],
    allowed_dev: set[str],
) -> None:
    unconditional_normal = set()
    for dependency in dependency_records(crate):
        name = dependency["name"]
        kind = dependency["kind"] or "normal"
        target = dependency["target"]
        if kind == "normal":
            if name not in required_normal:
                fail(f"{crate} dependency is not allowed: {name}")
            if target is None:
                unconditional_normal.add(name)
        elif kind == "dev":
            if name not in allowed_dev:
                fail(f"{crate} dev-dependency is not allowed: {name}")
        elif kind == "build":
            fail(f"{crate} build-dependency is not allowed: {name}")
        else:
            fail(f"{crate} has unknown dependency kind for {name}: {kind}")
    missing = required_normal - unconditional_normal
    if missing:
        fail(f"{crate} required dependency is missing: {sorted(missing)[0]}")


exact_dependencies(
    "context-codec",
    {"bytemuck", "context-core", "thiserror"},
    {"proptest"},
)
exact_dependencies(
    "context-query",
    {"context-core", "context-filter", "context-hybrid", "serde", "serde_json"},
    {"proptest"},
)
exact_dependencies("context-build", {"context-core"}, {"proptest"})
exact_dependencies(
    "pgcontext-worker",
    {"context-build", "context-core", "context-query", "serde", "serde_json", "sha2", "tokio"},
    {"proptest"},
)

for crate, forbidden in (
    (
        "context-index",
        {"context-storage", "context-query", "context-build"},
    ),
    (
        "context-storage",
        {"context-index", "context-query", "context-build"},
    ),
):
    for dependency in dependency_records(crate):
        if dependency["name"] in forbidden:
            fail(f"{crate} dependency is forbidden: {dependency['name']}")

postgres_packages = {
    "pgrx",
    "pgrx-tests",
    "postgres",
    "postgres-types",
    "tokio-postgres",
}
for crate in pure_crates:
    for dependency in dependency_records(crate):
        if dependency["name"] in postgres_packages:
            fail(f"PostgreSQL dependency leaked into pure crate manifest: {crate}")

pg_normal = {
    dependency["name"]
    for dependency in dependency_records("context-pg")
    if (dependency["kind"] or "normal") == "normal" and dependency["target"] is None
}
for required in ("context-query", "context-build"):
    if required not in pg_normal:
        fail(f"context-pg required dependency is missing: {required}")


def rust_sources(crate: str):
    crate_directory = {
        "pgcontext-worker": "context-rerank",
    }.get(crate, crate)
    crate_root = root / "crates" / crate_directory
    if not crate_root.is_dir():
        fail(f"missing crate directory: crates/{crate}")
    return sorted(
        path
        for path in crate_root.rglob("*.rs")
        if "target" not in path.relative_to(crate_root).parts
    )


postgres_import = re.compile(
    r"(?:\bpgrx\s*(?:::|\bas\b|[;{])|\bpg_sys\s*(?:::|\bas\b|[;{])|"
    r"\btokio_postgres\s*(?:::|\bas\b|[;{])|\bpostgres\s*(?:::|\bas\b|[;{])|"
    r"\bSpi::|\bPgSqlErrorCode\b|\bERRCODE_)"
)
sqlstate_literal = re.compile(r"(?P<quote>['\"])(?P<code>[A-Z0-9]{5})(?P=quote)")
sqlstate_method = re.compile(r"\.sqlstate\s*\(")


def line_number(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


for crate in pure_crates:
    for path in rust_sources(crate):
        text = path.read_text(encoding="utf-8")
        relative = path.relative_to(root)
        for match in postgres_import.finditer(text):
            line = text.rsplit("\n", 1)[-1]
            source_line = text.splitlines()[line_number(text, match.start()) - 1].lstrip()
            if source_line.startswith(("//", "///", "//!")):
                continue
            fail(
                f"PostgreSQL import leaked into pure crate source: "
                f"{relative}:{line_number(text, match.start())}"
            )
        method = sqlstate_method.search(text)
        if method:
            fail(
                f"SQLSTATE transport policy leaked into pure crate source: "
                f"{relative}:{line_number(text, method.start())}"
            )
        for match in sqlstate_literal.finditer(text):
            code = match.group("code")
            if not any(character.isdigit() for character in code):
                continue
            if relative.as_posix() == "crates/context-core/tests/bit_vector.rs" and set(code) <= {"0", "1"}:
                continue
            fail(
                f"SQLSTATE transport policy leaked into pure crate source: "
                f"{relative}:{line_number(text, match.start())}"
            )


source_forbidden = {
    "context-codec": re.compile(r"\bcontext_(?:filter|hybrid|index|storage|query|build)\b"),
    "context-query": re.compile(r"\bcontext_(?:index|storage|build)\b"),
    "context-index": re.compile(r"\bcontext_(?:storage|query|build)\b"),
    "context-storage": re.compile(r"\bcontext_(?:index|query|build)\b"),
    "context-build": re.compile(r"\bcontext_(?:filter|hybrid|index|storage|query)\b"),
    "pgcontext-worker": re.compile(r"\bcontext_(?:codec|filter|hybrid|index|storage|pg|test)\b"),
}
source_messages = {
    "context-codec": "context-codec source imports a sibling crate",
    "context-query": "context-query source imports an infrastructure crate",
    "context-index": "context-index source imports a forbidden sibling crate",
    "context-storage": "context-storage source imports a forbidden sibling crate",
    "context-build": "context-build source imports a crate other than context-core",
    "pgcontext-worker": "pgcontext-worker source imports a crate other than context-build/context-core/context-query",
}
for crate, pattern in source_forbidden.items():
    for path in rust_sources(crate):
        text = path.read_text(encoding="utf-8")
        match = pattern.search(text)
        if match:
            fail(
                f"{source_messages[crate]}: "
                f"{path.relative_to(root)}:{line_number(text, match.start())}"
            )


canonical_definitions = {
    re.compile(r"\bpub\s+enum\s+ScoreOrder\b"): Path("crates/context-core/src/retrieval.rs"),
    re.compile(r"\bpub\s+enum\s+VectorRepresentation\b"): Path("crates/context-core/src/vector.rs"),
    re.compile(r"\bpub\s+enum\s+IndexKind\b"): Path("crates/context-core/src/retrieval.rs"),
    re.compile(r"\bpub\s+enum\s+SourceAuthority\b"): Path("crates/context-core/src/retrieval.rs"),
    re.compile(r"(?:\bpub\s+struct\s+|\bnonzero_id!\(\s*)GenerationId\b"): Path("crates/context-core/src/retrieval.rs"),
    re.compile(r"(?:\bpub\s+struct\s+|\bnonzero_id!\(\s*)ConfigurationRevision\b"): Path("crates/context-core/src/retrieval.rs"),
    re.compile(r"(?:\bpub\s+struct\s+|\bnonzero_id!\(\s*)ProfileId\b"): Path("crates/context-core/src/retrieval.rs"),
    re.compile(r"(?:\bpub\s+struct\s+|\bnonzero_id!\(\s*)OccurrenceId\b"): Path("crates/context-core/src/retrieval.rs"),
    re.compile(r"(?:\bpub\s+struct\s+|\bnonzero_id!\(\s*)SourceVersion\b"): Path("crates/context-core/src/retrieval.rs"),
    re.compile(r"\bpub\s+enum\s+ReadinessReason\b"): Path("crates/context-core/src/retrieval.rs"),
    re.compile(r"\bpub\s+enum\s+Completion\b"): Path("crates/context-core/src/retrieval.rs"),
    re.compile(r"\bpub\s+enum\s+CandidateBranch\b"): Path("crates/context-query/src/types.rs"),
    re.compile(r"\bpub\s+enum\s+CandidateSourceKind\b"): Path("crates/context-query/src/types.rs"),
    re.compile(r"\bpub\s+struct\s+CandidateProvenance\b"): Path("crates/context-query/src/types.rs"),
    re.compile(r"\bpub\s+struct\s+CandidateDiagnostics\b"): Path("crates/context-query/src/types.rs"),
    re.compile(r"\bpub\s+struct\s+Candidate\b"): Path("crates/context-query/src/types.rs"),
    re.compile(r"\bpub\s+enum\s+RetrievalRegistrationError\b"): Path("crates/context-codec/src/registration.rs"),
    re.compile(r"\bpub\s+enum\s+CodecKind\b"): Path("crates/context-codec/src/lib.rs"),
    re.compile(r"\bpub\s+enum\s+TrainedQuantizer\b"): Path("crates/context-codec/src/training.rs"),
    re.compile(r"\bpub\s+enum\s+QuantizedCodebook\b"): Path("crates/context-codec/src/encoded.rs"),
    re.compile(r"\bpub\s+struct\s+PreparedQuantizedQuery\b"): Path("crates/context-codec/src/encoded.rs"),
    re.compile(r"\bpub\s+struct\s+ScalarQuantizer\b"): Path("crates/context-codec/src/quantization.rs"),
    re.compile(r"\bpub\s+struct\s+ProductQuantizer\b"): Path("crates/context-codec/src/quantization.rs"),
}
workspace_sources = sorted((root / "crates").glob("*/**/*.rs"))
for pattern, owner in canonical_definitions.items():
    definitions = []
    for path in workspace_sources:
        text = path.read_text(encoding="utf-8")
        if pattern.search(text):
            definitions.append(path.relative_to(root))
    if definitions != [owner]:
        fail(
            f"canonical retrieval definition ownership mismatch for {pattern.pattern}: "
            f"expected {owner}, got {definitions}"
        )

for path in workspace_sources:
    text = path.read_text(encoding="utf-8")
    match = re.search(r"\bScoreDirection\b", text)
    if match:
        fail(
            f"obsolete ScoreDirection definition or adapter remains: "
            f"{path.relative_to(root)}:{line_number(text, match.start())}"
        )

filesystem_api = re.compile(
    r"\bstd::(?:fs|path)\b|\bfs::|\bstd::\{[^}]*\b(?:fs|path)\b|"
    r"\bOpenOptions\b|\bFile::(?:open|create)\b"
)
for crate in ("context-query", "context-index"):
    for path in rust_sources(crate):
        text = path.read_text(encoding="utf-8")
        match = filesystem_api.search(text)
        if match:
            fail(
                f"filesystem API leaked into {crate}: "
                f"{path.relative_to(root)}:{line_number(text, match.start())}"
            )
PY
