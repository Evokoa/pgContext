#!/usr/bin/env python3
"""Token-aware HNSW callback and unsafe-item inventory checker.

This intentionally uses only the Python standard library so the guard remains
available on contributor and release hosts without adding build dependencies.
It is not a general Rust parser. It recognizes the item, attribute, function,
impl, literal, and delimited-expression forms used by the guarded HNSW modules,
and fails closed when the required contract does not have that shape.
"""

from __future__ import annotations

import dataclasses
import pathlib
import re
import sys
from collections.abc import Iterable, Sequence


EXPECTED_CALLBACKS = 17
EXPECTED_AM_CALLBACKS = 14
EXPECTED_UNSAFE_ITEMS = 108
EXPECTED_INCLUDES = {
    ("hnsw_am.rs", "hnsw_am_callbacks.rs"),
    ("hnsw_am.rs", "hnsw_am_scan_callbacks.rs"),
    ("hnsw_am.rs", "hnsw_am_compaction.rs"),
    ("hnsw_am.rs", "hnsw_am_metapage.rs"),
    ("hnsw_am.rs", "hnsw_am_page_storage.rs"),
    ("hnsw_am.rs", "hnsw_am_packed_cache.rs"),
    ("hnsw_am.rs", "hnsw_am_graph_read.rs"),
    ("hnsw_am.rs", "hnsw_am_graph_scan.rs"),
    ("hnsw_am.rs", "hnsw_am/mapped_files.rs"),
    ("hnsw_am.rs", "hnsw_am/mapped_lifecycle.rs"),
    ("hnsw_am.rs", "hnsw_am/sql_contract.rs"),
    ("hnsw_am.rs", "hnsw_am/shared_registry.rs"),
    ("hnsw_am.rs", "hnsw_am_validation.rs"),
    ("hnsw_am/mvcc_contract.rs", "mvcc_contract/tests.rs"),
    ("hnsw_am/wal_contract.rs", "wal_contract/tests.rs"),
}


class CheckError(Exception):
    """A deterministic contract violation reported to the shell guard."""


@dataclasses.dataclass(frozen=True)
class Token:
    kind: str
    value: str
    start: int
    end: int
    line: int


@dataclasses.dataclass(frozen=True)
class LineComment:
    text: str
    start: int
    end: int


@dataclasses.dataclass
class RustSource:
    path: pathlib.Path
    relative: str
    text: str
    tokens: list[Token]
    comments: list[LineComment]
    pairs: dict[int, int]


@dataclasses.dataclass(frozen=True)
class Function:
    source: RustSource
    name: str
    start_index: int
    fn_index: int
    body_open: int
    body_close: int
    unsafe: bool
    abi: str | None
    owner: str | None


def fail(message: str) -> None:
    raise CheckError(message)


def lex_rust(path: pathlib.Path, relative: str) -> RustSource:
    text = path.read_text(encoding="utf-8")
    tokens: list[Token] = []
    comments: list[LineComment] = []
    i = 0
    line = 1
    size = len(text)

    def advance(end: int) -> None:
        nonlocal i, line
        line += text.count("\n", i, end)
        i = end

    while i < size:
        char = text[i]
        if char.isspace():
            advance(i + 1)
            continue
        if text.startswith("//", i):
            end = text.find("\n", i + 2)
            end = size if end == -1 else end
            comments.append(LineComment(text[i + 2 : end], i, end))
            advance(end)
            continue
        if text.startswith("/*", i):
            start = i
            depth = 1
            advance(i + 2)
            while i < size and depth:
                if text.startswith("/*", i):
                    depth += 1
                    advance(i + 2)
                elif text.startswith("*/", i):
                    depth -= 1
                    advance(i + 2)
                else:
                    advance(i + 1)
            if depth:
                fail(f"unterminated block comment in {relative} at byte {start}")
            continue

        raw_match = re.match(r"(?:b|c)?r(#{0,255})\"", text[i:])
        if raw_match:
            hashes = raw_match.group(1)
            prefix_end = i + raw_match.end()
            terminator = '"' + hashes
            end_quote = text.find(terminator, prefix_end)
            if end_quote == -1:
                fail(f"unterminated raw string in {relative} at line {line}")
            end = end_quote + len(terminator)
            tokens.append(Token("string", text[prefix_end:end_quote], i, end, line))
            advance(end)
            continue

        string_prefix = 1 if char in "bc" and i + 1 < size and text[i + 1] == '"' else 0
        if char == '"' or string_prefix:
            start = i
            quote = i + string_prefix
            end = quote + 1
            escaped = False
            while end < size:
                current = text[end]
                if current == '"' and not escaped:
                    end += 1
                    break
                if current == "\\" and not escaped:
                    escaped = True
                else:
                    escaped = False
                end += 1
            else:
                fail(f"unterminated string in {relative} at line {line}")
            value = text[quote + 1 : end - 1]
            tokens.append(Token("string", value, start, end, line))
            advance(end)
            continue

        if char == "'":
            char_match = re.match(r"'(?:\\.|[^'\\\n])'", text[i:])
            if char_match:
                end = i + char_match.end()
                tokens.append(Token("char", text[i:end], i, end, line))
                advance(end)
                continue

        ident_match = re.match(r"[A-Za-z_][A-Za-z0-9_]*", text[i:])
        if ident_match:
            end = i + ident_match.end()
            tokens.append(Token("ident", text[i:end], i, end, line))
            advance(end)
            continue
        number_match = re.match(r"[0-9][A-Za-z0-9_.]*", text[i:])
        if number_match:
            end = i + number_match.end()
            tokens.append(Token("number", text[i:end], i, end, line))
            advance(end)
            continue

        punct = next(
            (candidate for candidate in ("::", "->", "=>", "..=", "..", "&&", "||") if text.startswith(candidate, i)),
            char,
        )
        end = i + len(punct)
        tokens.append(Token("punct", punct, i, end, line))
        advance(end)

    pairs: dict[int, int] = {}
    stack: list[tuple[str, int]] = []
    closing = {")": "(", "]": "[", "}": "{"}
    for index, token in enumerate(tokens):
        if token.value in "([{":
            stack.append((token.value, index))
        elif token.value in closing:
            if not stack or stack[-1][0] != closing[token.value]:
                fail(f"unbalanced delimiter in {relative} at line {token.line}")
            _, opening = stack.pop()
            pairs[opening] = index
            pairs[index] = opening
    if stack:
        _, opening = stack[-1]
        fail(f"unclosed delimiter in {relative} at line {tokens[opening].line}")
    return RustSource(path, relative, text, tokens, comments, pairs)


def function_header_start(tokens: Sequence[Token], fn_index: int) -> int:
    index = fn_index - 1
    while index >= 0 and tokens[index].value not in (";", "{", "}"):
        index -= 1
    return index + 1


def function_body(source: RustSource, fn_index: int) -> tuple[int, int]:
    tokens = source.tokens
    nested_delimiters = 0
    for index in range(fn_index + 1, len(tokens)):
        if tokens[index].value in ("(", "["):
            nested_delimiters += 1
            continue
        if tokens[index].value in (")", "]"):
            nested_delimiters -= 1
            continue
        if tokens[index].value == ";" and nested_delimiters == 0:
            fail(f"function declaration without body in checked HNSW source: {tokens[fn_index + 1].value}")
        if tokens[index].value == "{" and nested_delimiters == 0:
            return index, source.pairs[index]
    fail(f"function body missing in {source.relative} at line {tokens[fn_index].line}")


def canonical(tokens: Sequence[Token]) -> str:
    value = " ".join(token.value for token in tokens)
    for pattern, replacement in (
        (r"\s*::\s*", "::"),
        (r"\s*<\s*", "<"),
        (r"\s*>\s*", ">"),
        (r"\s*,\s*", ", "),
        (r"\s+", " "),
    ):
        value = re.sub(pattern, replacement, value)
    return value.strip()


def impl_ranges(source: RustSource) -> tuple[list[tuple[int, int, str]], list[tuple[int, int, str]]]:
    ranges: list[tuple[int, int, str]] = []
    unsafe_impls: list[tuple[int, int, str]] = []
    tokens = source.tokens
    for index, token in enumerate(tokens):
        if token.value != "impl":
            continue
        body_open = next((cursor for cursor in range(index + 1, len(tokens)) if tokens[cursor].value == "{"), None)
        if body_open is None:
            fail(f"impl body missing in {source.relative} at line {token.line}")
        header = list(tokens[index + 1 : body_open])
        for_position = next((position for position, item in enumerate(header) if item.value == "for"), None)
        owner_tokens = header[for_position + 1 :] if for_position is not None else header
        while owner_tokens and owner_tokens[0].value == "<":
            depth = 0
            consumed = 0
            for consumed, item in enumerate(owner_tokens, start=1):
                depth += item.value == "<"
                depth -= item.value == ">"
                if depth == 0:
                    break
            owner_tokens = owner_tokens[consumed:]
        owner = next((item.value for item in owner_tokens if item.kind == "ident"), "")
        if not owner:
            fail(f"cannot identify impl owner in {source.relative} at line {token.line}")
        body_close = source.pairs[body_open]
        ranges.append((body_open, body_close, owner))
        if index > 0 and tokens[index - 1].value == "unsafe":
            unsafe_impls.append((index - 1, body_open, canonical(header)))
    return ranges, unsafe_impls


def parse_functions(source: RustSource) -> tuple[list[Function], list[tuple[int, int, str]]]:
    ranges, unsafe_impls = impl_ranges(source)
    functions: list[Function] = []
    tokens = source.tokens
    for fn_index, token in enumerate(tokens):
        if token.value != "fn" or fn_index + 1 >= len(tokens) or tokens[fn_index + 1].kind != "ident":
            continue
        start = function_header_start(tokens, fn_index)
        qualifier_start = start
        for position in range(start, fn_index):
            if tokens[position].value == "]":
                qualifier_start = position + 1
        header = tokens[qualifier_start:fn_index]
        body_open, body_close = function_body(source, fn_index)
        abi = None
        for position, item in enumerate(header):
            if item.value == "extern":
                abi = header[position + 1].value if position + 1 < len(header) and header[position + 1].kind == "string" else "C"
        owner_candidates = [entry for entry in ranges if entry[0] < fn_index < entry[1]]
        owner = min(owner_candidates, key=lambda entry: entry[1] - entry[0])[2] if owner_candidates else None
        functions.append(
            Function(
                source=source,
                name=tokens[fn_index + 1].value,
                start_index=start,
                fn_index=fn_index,
                body_open=body_open,
                body_close=body_close,
                unsafe=any(item.value == "unsafe" for item in header),
                abi=abi,
                owner=owner,
            )
        )
    return functions, unsafe_impls


def parse_fields(source: RustSource, opening: int) -> dict[str, list[Token]]:
    tokens = source.tokens
    closing = source.pairs[opening]
    fields: dict[str, list[Token]] = {}
    cursor = opening + 1
    while cursor < closing:
        if tokens[cursor].value == ",":
            cursor += 1
            continue
        if tokens[cursor].value == "..":
            break
        if tokens[cursor].kind != "ident" or cursor + 1 >= closing or tokens[cursor + 1].value != ":":
            fail(f"unsupported direct struct field in {source.relative} at line {tokens[cursor].line}")
        name = tokens[cursor].value
        cursor += 2
        start = cursor
        stack: list[str] = []
        while cursor < closing:
            value = tokens[cursor].value
            if not stack and value == ",":
                break
            if value in "([{<":
                stack.append(value)
            elif value in ")]}>" and stack:
                stack.pop()
            cursor += 1
        if name in fields:
            fail(f"duplicate direct struct field {name} in {source.relative}")
        fields[name] = list(tokens[start:cursor])
        if cursor < closing and tokens[cursor].value == ",":
            cursor += 1
    return fields


def callback_contracts(source: RustSource) -> list[tuple[str, str, str]]:
    tokens = source.tokens
    const_positions = [
        index
        for index, token in enumerate(tokens)
        if token.value == "HNSW_CALLBACK_CONTRACTS"
        and index + 1 < len(tokens)
        and tokens[index + 1].value == ":"
        and any(
            item.value == "const"
            for item in tokens[function_header_start(tokens, index) : index]
        )
    ]
    if len(const_positions) != 1:
        fail("HNSW callback inventory constant must have exactly one definition")
    start = const_positions[0]
    array_open = next((index for index in range(start, len(tokens)) if tokens[index].value == "[" and index > start and tokens[index - 1].value == "="), None)
    if array_open is None:
        fail("HNSW callback inventory array initializer is missing")
    array_close = source.pairs[array_open]
    result: list[tuple[str, str, str]] = []
    cursor = array_open + 1
    while cursor < array_close:
        if tokens[cursor].value == ",":
            cursor += 1
            continue
        if not (
            tokens[cursor].value == "HnswCallbackContract"
            and cursor + 1 < array_close
            and tokens[cursor + 1].value == "{"
        ):
            fail(
                "HNSW callback inventory entries must be direct "
                f"HnswCallbackContract literals at line {tokens[cursor].line}"
            )
        fields = parse_fields(source, cursor + 1)
        try:
            callback_value = fields["callback"]
            safe_value = fields["safe_inner"]
            class_value = fields["class"]
        except KeyError as error:
            fail(f"malformed callback inventory entry missing {error.args[0]}")
        if len(callback_value) != 1 or callback_value[0].kind != "string":
            fail("callback inventory name must be a direct string literal")
        if len(safe_value) != 1 or safe_value[0].kind != "string":
            fail("callback safe-function name must be a direct string literal")
        class_names = [item.value for item in class_value if item.kind == "ident"]
        if len(class_names) < 2 or class_names[-2] != "HnswCallbackClass":
            fail("callback class must be a direct HnswCallbackClass variant")
        result.append((callback_value[0].value, safe_value[0].value, class_names[-1]))
        cursor = source.pairs[cursor + 1] + 1
    return result


def callback_attribute_and_contract(function: Function) -> None:
    tokens = function.source.tokens
    guard_starts = [
        index
        for index in range(function.start_index, function.fn_index)
        if [token.value for token in tokens[index : index + 4]]
        == ["#", "[", "pg_guard", "]"]
    ]
    if len(guard_starts) != 1:
        fail(f"HNSW callback is missing #[pg_guard]: {function.name}")
    header_values = [
        token.value for token in tokens[function.start_index : function.fn_index]
    ]
    if any(
        header_values[index : index + 3] in (["#", "[", "cfg"], ["#", "[", "cfg_attr"])
        for index in range(max(0, len(header_values) - 2))
    ):
        fail(f"HNSW callback must not be conditionally compiled: {function.name}")
    guard_end = tokens[guard_starts[0] + 3].end
    fn_start = tokens[function.fn_index].start
    safety = [
        comment
        for comment in function.source.comments
        if guard_end <= comment.start < fn_start and comment.text.lstrip().startswith("SAFETY:")
    ]
    if not safety:
        fail(f"HNSW callback is missing a local SAFETY contract: {function.name}")


def validates_final_delegation(function: Function, safe_inner: str) -> bool:
    tokens = function.source.tokens
    body = tokens[function.body_open + 1 : function.body_close]
    if sum(item.kind == "ident" and item.value == safe_inner for item in body) != 1:
        return False
    if body and body[-1].value == ";":
        body = body[:-1]
    last_separator = -1
    depths = {"(": 0, "[": 0, "{": 0}
    matching = {")": "(", "]": "[", "}": "{"}
    for position, token in enumerate(body):
        if token.value in depths:
            depths[token.value] += 1
        elif token.value in matching:
            depths[matching[token.value]] -= 1
        elif token.value == ";" and not any(depths.values()):
            last_separator = position
    expression = body[last_separator + 1 :]
    if (
        len(expression) < 5
        or [item.value for item in expression[:4]] != ["self", "::", safe_inner, "("]
    ):
        return False
    opening_token = expression[3]
    closing_token = expression[-1]
    opening = next(
        index
        for index in range(function.body_open + 1, function.body_close)
        if tokens[index] is opening_token
    )
    closing = next(
        index
        for index in range(function.body_open + 1, function.body_close)
        if tokens[index] is closing_token
    )
    return function.source.pairs.get(opening) == closing and expression[-1].value == ")"


def routine_callbacks(source: RustSource, functions: Sequence[Function]) -> list[str]:
    routines = [function for function in functions if function.name == "hnsw_index_am_routine"]
    if len(routines) != 1:
        fail("hnsw_index_am_routine must have exactly one definition")
    routine = routines[0]
    tokens = source.tokens
    literals: list[int] = []
    depth = 0
    for index in range(routine.body_open + 1, routine.body_close):
        value = tokens[index].value
        if value == "{" and depth == 0 and index > 0 and tokens[index - 1].value == "IndexAmRoutine":
            literals.append(index)
        if value == "{":
            depth += 1
        elif value == "}":
            depth -= 1
    if len(literals) != 1:
        fail("hnsw_index_am_routine must directly construct exactly one IndexAmRoutine")
    fields = parse_fields(source, literals[0])
    result: list[str] = []
    for name, value in fields.items():
        if not name.startswith("am") or len(value) < 3 or value[0].value != "Some" or value[1].value != "(":
            continue
        if value[-1].value != ")":
            fail(f"IndexAmRoutine callback field has unsupported value: {name}")
        callback_path = value[2:-1]
        if not callback_path or any(
            (position % 2 == 0 and item.kind != "ident")
            or (position % 2 == 1 and item.value != "::")
            for position, item in enumerate(callback_path)
        ):
            fail(f"IndexAmRoutine callback field has unsupported value: {name}")
        result.append(callback_path[-1].value)
    return result


def unsafe_inventory(path: pathlib.Path) -> set[tuple[str, str, str]]:
    result: set[tuple[str, str, str]] = set()
    count = 0
    for line_number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        fields = raw.split("|")
        if len(fields) != 4 or any(not field.strip() for field in fields):
            fail(f"malformed HNSW unsafe inventory row at line {line_number}")
        relative, kind, symbol, _reason = (field.strip() for field in fields)
        if kind not in {"extern", "fn", "impl"}:
            fail(f"invalid HNSW unsafe inventory kind at line {line_number}: {kind}")
        item = (relative, kind, symbol)
        if item in result:
            fail(f"duplicate HNSW unsafe inventory row at line {line_number}")
        result.add(item)
        count += 1
    if count != EXPECTED_UNSAFE_ITEMS:
        fail(f"HNSW unsafe inventory count mismatch: {count} != {EXPECTED_UNSAFE_ITEMS}")
    return result


def source_unsafe_items(
    parsed: Sequence[tuple[RustSource, Sequence[Function], Sequence[tuple[int, int, str]]]],
) -> set[tuple[str, str, str]]:
    result: set[tuple[str, str, str]] = set()
    for source, functions, unsafe_impls in parsed:
        for function in functions:
            if not function.unsafe:
                continue
            kind = "extern" if function.abi is not None else "fn"
            name = f"{function.owner}::{function.name}" if function.owner else function.name
            result.add((source.relative, kind, name))
        for _start, _opening, signature in unsafe_impls:
            result.add((source.relative, "impl", signature))
    return result


def lexical_brace_depth(source: RustSource, token_index: int) -> int:
    depth = 0
    for token in source.tokens[:token_index]:
        if token.value == "{":
            depth += 1
        elif token.value == "}":
            depth -= 1
    return depth


def require_unconditional_safe_function(function: Function) -> None:
    header = function.source.tokens[function.start_index : function.fn_index]
    if (
        any(token.value == "#" for token in header)
        or function.owner is not None
        or lexical_brace_depth(function.source, function.fn_index) != 0
    ):
        fail(
            "HNSW safe function must be an unconditional top-level definition: "
            f"{function.name}"
        )


def reject_inventory_name_imports(
    sources: Iterable[RustSource], protected_names: set[str]
) -> None:
    for source in sources:
        tokens = source.tokens
        for index, token in enumerate(tokens):
            if token.value not in {"use", "extern"}:
                continue
            if token.value == "extern" and (
                index + 1 >= len(tokens) or tokens[index + 1].value != "crate"
            ):
                continue
            end = next(
                (cursor for cursor in range(index + 1, len(tokens)) if tokens[cursor].value == ";"),
                len(tokens),
            )
            bound = protected_names.intersection(
                item.value for item in tokens[index + 1 : end] if item.kind == "ident"
            )
            if bound:
                fail(
                    "HNSW checked sources must not import an inventoried name: "
                    f"{source.relative}:{token.line}: {sorted(bound)[0]}"
                )


def reject_local_macro_definitions(sources: Iterable[RustSource]) -> None:
    for source in sources:
        tokens = source.tokens
        for index, token in enumerate(tokens):
            macro_rules = (
                token.value == "macro_rules"
                and index + 1 < len(tokens)
                and tokens[index + 1].value == "!"
            )
            declarative_macro = (
                token.value == "macro"
                and index + 1 < len(tokens)
                and tokens[index + 1].kind == "ident"
            )
            if macro_rules or declarative_macro:
                fail(
                    "HNSW checked sources must not define macros that can hide "
                    f"unsafe items: {source.relative}:{token.line}"
                )


def check_source_loading_inventory(sources: Iterable[RustSource]) -> None:
    actual: set[tuple[str, str]] = set()
    for source in sources:
        tokens = source.tokens
        for index, token in enumerate(tokens):
            if (
                token.value == "#"
                and index + 1 < len(tokens)
                and tokens[index + 1].value == "["
            ):
                attribute_close = source.pairs[index + 1]
                if any(
                    item.kind == "ident" and item.value == "path"
                    for item in tokens[index + 2 : attribute_close]
                ):
                    fail(
                        "HNSW checked sources must not redirect modules with a path attribute: "
                        f"{source.relative}:{token.line}"
                    )
            if token.value != "include":
                continue
            invocation = tokens[index : index + 6]
            if (
                len(invocation) != 6
                or [item.value for item in invocation[:3]] != ["include", "!", "("]
                or invocation[3].kind != "string"
                or [item.value for item in invocation[4:]] != [")", ";"]
            ):
                fail(
                    "HNSW include! must use one direct reviewed string target: "
                    f"{source.relative}:{token.line}"
                )
            item = (source.relative, invocation[3].value)
            if item in actual:
                fail(f"duplicate HNSW include! invocation: {source.relative}:{token.line}")
            actual.add(item)
    if actual != EXPECTED_INCLUDES:
        missing = sorted(EXPECTED_INCLUDES - actual)
        unexpected = sorted(actual - EXPECTED_INCLUDES)
        details = []
        if missing:
            details.append(f"missing={missing}")
        if unexpected:
            details.append(f"unexpected={unexpected}")
        fail("HNSW include! inventory mismatch: " + " ".join(details))


def check_generic_wal_boundary(sources: Iterable[RustSource]) -> None:
    expected_source = "hnsw_am/wal_contract/critical_section.rs"
    for function_name in ("GenericXLogRegisterBuffer", "GenericXLogFinish"):
        calls: list[tuple[str, int]] = []
        expected = ["pg_sys", "::", function_name, "("]
        for source in sources:
            for index, token in enumerate(source.tokens):
                values = [item.value for item in source.tokens[index : index + len(expected)]]
                if values == expected:
                    calls.append((source.relative, token.line))
        if len(calls) != 1 or calls[0][0] != expected_source:
            fail(
                f"{function_name} must have exactly one call inside the linear "
                f"WAL permit boundary: calls={calls}"
            )


def display_diff(expected: set[tuple[str, str, str]], actual: set[tuple[str, str, str]]) -> str:
    lines: list[str] = []
    for item in sorted(expected - actual):
        lines.append("-" + "|".join(item))
    for item in sorted(actual - expected):
        lines.append("+" + "|".join(item))
    return "\n".join(lines)


def main(arguments: Sequence[str]) -> None:
    if len(arguments) != 5:
        fail("usage: checker HNSW_AM CONTRACT MODULE_ROOT PAGE_STORAGE UNSAFE_INVENTORY")
    am_path, contract_path, module_root, page_path, inventory_path = map(pathlib.Path, arguments)
    for path in (am_path, contract_path, module_root, page_path, inventory_path):
        if not path.exists():
            fail(f"HNSW callback guard input is missing: {path}")
    if not module_root.is_dir():
        fail(f"HNSW callback guard module root is missing: {module_root}")

    sources = [lex_rust(am_path, "hnsw_am.rs"), lex_rust(page_path, "hnsw_am_page_storage.rs")]
    for sibling_name in ("hnsw_am_packed_cache.rs", "hnsw_am_graph_read.rs", "hnsw_am_graph_scan.rs"):
        sibling_path = am_path.parent / sibling_name
        if not sibling_path.is_file():
            fail(f"HNSW reviewed include target is missing: {sibling_path}")
        sources.append(lex_rust(sibling_path, sibling_name))
    for included_name in (
        "hnsw_am_callbacks.rs",
        "hnsw_am_scan_callbacks.rs",
        "hnsw_am_compaction.rs",
        "hnsw_am_validation.rs",
        "hnsw_am_metapage.rs",
    ):
        included_path = am_path.parent / included_name
        if not included_path.is_file():
            fail(f"HNSW reviewed include target is missing: {included_path}")
        # include! gives these definitions the parent module's semantic path;
        # retain that path so the unsafe inventory stays tied to the public AM
        # boundary while still parsing the physical source file.
        sources.append(lex_rust(included_path, "hnsw_am.rs"))
    sources.extend(
        lex_rust(path, "hnsw_am/" + str(path.relative_to(module_root)))
        for path in sorted(module_root.rglob("*.rs"))
    )
    main_source = sources[0]
    reject_local_macro_definitions(sources)
    check_source_loading_inventory(sources)
    check_generic_wal_boundary(sources)

    parsed = []
    all_functions: list[Function] = []
    for source in sources:
        functions, unsafe_impls = parse_functions(source)
        parsed.append((source, functions, unsafe_impls))
        all_functions.extend(functions)

    contract_source = next(source for source in sources if source.path == contract_path)
    inventory = callback_contracts(contract_source)
    if len(inventory) != EXPECTED_CALLBACKS:
        fail(f"HNSW callback inventory count mismatch: {len(inventory)} != {EXPECTED_CALLBACKS}")
    callback_names = [entry[0] for entry in inventory]
    safe_names = [entry[1] for entry in inventory]
    if len(set(callback_names)) != len(callback_names):
        fail("duplicate HNSW callback inventory entry")
    if len(set(safe_names)) != len(safe_names):
        fail("duplicate HNSW safe-function inventory entry")
    inventory_am = sorted(entry[0] for entry in inventory if entry[2] == "AccessMethod")
    if len(inventory_am) != EXPECTED_AM_CALLBACKS:
        fail(f"HNSW AM callback inventory count mismatch: {len(inventory_am)} != {EXPECTED_AM_CALLBACKS}")

    source_callbacks = sorted(
        function.name
        for function in all_functions
        if function.unsafe and function.abi == "C-unwind" and function.name.startswith("pgcontext_hnsw_")
    )
    if sorted(callback_names) != source_callbacks:
        fail("HNSW unsafe callback source does not match the executable inventory")

    routine = sorted(routine_callbacks(main_source, parsed[0][1]))
    if len(routine) != EXPECTED_AM_CALLBACKS:
        fail(f"HNSW IndexAmRoutine callback count mismatch: {len(routine)} != {EXPECTED_AM_CALLBACKS}")
    if routine != inventory_am:
        fail("HNSW IndexAmRoutine callbacks do not match AccessMethod inventory entries")

    for callback, safe_inner, _callback_class in inventory:
        callback_matches = [
            function
            for function in all_functions
            if function.name == callback and function.unsafe and function.abi == "C-unwind"
        ]
        if len(callback_matches) != 1:
            fail(f"HNSW callback must have exactly one unsafe C-unwind definition: {callback}")
        wrapper = callback_matches[0]
        callback_attribute_and_contract(wrapper)
        safe_matches = [function for function in all_functions if function.name == safe_inner]
        if len(safe_matches) != 1:
            fail(f"HNSW callback must pair with exactly one safe function: {callback} -> {safe_inner}")
        safe = safe_matches[0]
        if safe.unsafe:
            fail(f"HNSW callback must pair with a safe function: {callback} -> {safe_inner}")
        require_unconditional_safe_function(safe)
        if safe.source.relative != wrapper.source.relative:
            fail(f"HNSW safe function must follow its wrapper in the same module: {callback} -> {safe_inner}")
        if safe.source.path == wrapper.source.path and safe.fn_index <= wrapper.fn_index:
            fail(f"HNSW safe function must follow its wrapper in the same module: {callback} -> {safe_inner}")
        if not validates_final_delegation(wrapper, safe_inner):
            fail(f"HNSW callback does not delegate to its safe function: {callback} -> {safe_inner}")

    reject_inventory_name_imports(sources, set(callback_names) | set(safe_names))

    safe_externs = [
        function for function in all_functions if not function.unsafe and function.abi == "C-unwind"
    ]
    expected_finfos = {
        "pg_finfo_pgcontext_hnsw_handler": "HNSW_HANDLER_FINFO",
        "pg_finfo_pgcontext_hnsw_mapped_sql_drop": "MAPPED_SQL_DROP_FINFO",
    }
    if {function.name for function in safe_externs} != set(expected_finfos):
        names = ", ".join(function.name for function in safe_externs) or "<empty>"
        fail(f"unexpected safe HNSW C-unwind export inventory: {names}")
    for finfo in safe_externs:
        finfo_body = finfo.source.tokens[finfo.body_open + 1 : finfo.body_close]
        if [token.value for token in finfo_body] != ["&", expected_finfos[finfo.name]]:
            fail("HNSW finfo exemption must return its immutable static record")

    expected_unsafe = unsafe_inventory(inventory_path)
    actual_unsafe = source_unsafe_items(parsed)
    if len(actual_unsafe) != EXPECTED_UNSAFE_ITEMS:
        fail(
            f"HNSW unsafe source count mismatch: {len(actual_unsafe)} != {EXPECTED_UNSAFE_ITEMS}\n"
            + display_diff(expected_unsafe, actual_unsafe)
        )
    if actual_unsafe != expected_unsafe:
        fail("HNSW unsafe functions/impls do not match the checked-in inventory\n" + display_diff(expected_unsafe, actual_unsafe))

    print(
        f"HNSW callback/unsafe inventory passed ({EXPECTED_CALLBACKS} callbacks, "
        f"{EXPECTED_UNSAFE_ITEMS} unsafe items)"
    )


if __name__ == "__main__":
    try:
        main(sys.argv[1:])
    except (CheckError, OSError) as error:
        print(error, file=sys.stderr)
        raise SystemExit(1) from None
