#!/usr/bin/env python3
"""Validate IVFFlat guarded callbacks and the unsafe-function inventory."""

from __future__ import annotations

import re
import sys
from pathlib import Path


CALLBACK_RE = re.compile(
    r"(?:pub(?:\([^)]*\))?\s+)?unsafe\s+extern\s+\"C-unwind\"\s+fn\s+"
    r"(?P<name>[A-Za-z0-9_]+)[^{]*\{",
    re.MULTILINE,
)
UNSAFE_FN_RE = re.compile(
    r"(?:pub(?:\([^)]*\))?\s+)?unsafe\s+(?:extern\s+\"C-unwind\"\s+)?fn\s+"
    r"(?P<name>[A-Za-z0-9_]+)[^{]*\{",
    re.MULTILINE,
)
CONTRACT_RE = re.compile(r'contract\(\s*"([A-Za-z0-9_]+)"')


def fail(message: str) -> None:
    raise SystemExit(message)


def body(source: str, opening: int) -> str:
    depth = 0
    for offset in range(opening, len(source)):
        char = source[offset]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return source[opening : offset + 1]
    fail("unterminated Rust function body")
    return ""


def main() -> None:
    if len(sys.argv) != 6:
        fail("usage: checker MAIN EXTERNAL OPTIONS CONTRACT INVENTORY")
    source_paths = [Path(value) for value in sys.argv[1:4]]
    contract_path = Path(sys.argv[4])
    inventory_path = Path(sys.argv[5])
    sources = {path: path.read_text(encoding="utf-8") for path in source_paths}

    callbacks: set[str] = set()
    unsafe_functions: set[tuple[str, str]] = set()
    for path, source in sources.items():
        for match in CALLBACK_RE.finditer(source):
            name = match.group("name")
            callbacks.add(name)
            preamble = source[max(0, match.start() - 600) : match.start()]
            if preamble.rfind("#[pg_guard]") < preamble.rfind('extern "C-unwind" fn'):
                fail(f"IVFFlat callback {name} is missing #[pg_guard]")
            function_body = body(source, source.find("{", match.start()))
            if "PgCallbackScope::new()" not in function_body:
                fail(f"IVFFlat callback {name} is missing PgCallbackScope")
        relative = "ivfflat_am.rs" if path.name == "ivfflat_am.rs" else f"ivfflat_am/{path.name}"
        for match in UNSAFE_FN_RE.finditer(source):
            unsafe_functions.add((relative, match.group("name")))

    contract_source = contract_path.read_text(encoding="utf-8")
    contracts = set(CONTRACT_RE.findall(contract_source))
    if callbacks != contracts:
        fail(
            "IVFFlat callback contract drift: "
            f"missing={sorted(callbacks - contracts)} extra={sorted(contracts - callbacks)}"
        )

    inventory: set[tuple[str, str]] = set()
    for line_number, line in enumerate(
        inventory_path.read_text(encoding="utf-8").splitlines(), start=1
    ):
        if not line or line.startswith("#"):
            continue
        fields = line.split("|", 2)
        if len(fields) != 3 or not fields[2]:
            fail(f"malformed IVFFlat unsafe inventory row {line_number}")
        key = (fields[0], fields[1])
        if key in inventory:
            fail(f"duplicate IVFFlat unsafe inventory row {line_number}")
        inventory.add(key)
    if inventory != unsafe_functions:
        fail(
            "IVFFlat unsafe inventory drift: "
            f"missing={sorted(unsafe_functions - inventory)} "
            f"extra={sorted(inventory - unsafe_functions)}"
        )
    print(
        f"IVFFlat callback/unsafe inventory passed "
        f"({len(callbacks)} callbacks, {len(unsafe_functions)} unsafe functions)"
    )


if __name__ == "__main__":
    main()
