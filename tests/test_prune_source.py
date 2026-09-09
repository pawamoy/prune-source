from __future__ import annotations

import ast
import inspect
import sys

from prune_source import RUFF_VERSION, prune_source


def test_accepts_only_source() -> None:
    assert tuple(inspect.signature(prune_source).parameters) == ("source",)


def test_prunes_function_body_into_compilable_source() -> None:
    source = 'def answer() -> int:\n    """Return the answer."""\n    return 42\n'

    pruned = prune_source(source)

    assert pruned is not None
    assert "Return the answer." in pruned
    assert "return 42" not in pruned
    tree = compile(pruned, "example.py", "exec", ast.PyCF_ONLY_AST, optimize=1)
    assert isinstance(tree, ast.Module)
    assert isinstance(tree.body[0], ast.FunctionDef)


def test_pruned_multiline_body_uses_empty_lines() -> None:
    source = "def answer():\n    value = (\n        40 + 2\n    )\n"

    pruned = prune_source(source)

    assert pruned == "def answer():\n\n\n    pass\n"


def test_keeps_class_init_implementation() -> None:
    source = (
        "class Example:\n"
        "    def __init__(self):\n"
        "        def nested():\n"
        "            removed = 1\n"
        "        self.value = 42\n"
    )

    pruned = prune_source(source)

    assert pruned is not None
    assert "self.value = 42" in pruned
    assert "removed = 1" not in pruned
    compile(pruned, "example.py", "exec", ast.PyCF_ONLY_AST, optimize=1)


def test_keeps_nfkc_normalized_class_init_implementation() -> None:
    source = (
        "class Example:\n"
        "    def __𝒊nit__(self):\n"
        "        self.value = 42\n"
        "    def method(self):\n"
        "        removed = 1\n"
    )

    pruned = prune_source(source)

    assert pruned is not None
    tree = ast.parse(pruned)
    class_definition = tree.body[0]
    assert isinstance(class_definition, ast.ClassDef)
    init = class_definition.body[0]
    assert isinstance(init, ast.FunctionDef)
    assert init.name == "__init__"
    assert isinstance(init.body[0], ast.Assign)
    assert "self.value = 42" in pruned
    assert "removed = 1" not in pruned


def test_prunes_soft_keyword_method_implementations() -> None:
    for method_name in ("case", "match", "type", "lazy"):
        source = (
            "class Example:\n"
            f"    def {method_name}(__init__):\n"
            "        implementation = 1\n"
        )

        pruned = prune_source(source)

        assert pruned is not None
        assert "implementation" not in pruned
        compile(pruned, "example.py", "exec", ast.PyCF_ONLY_AST, optimize=1)


def test_handles_inline_suite_with_explicit_line_continuation() -> None:
    source = "def predicate(): return first or \\\n    second\nvalue = 1\n"

    pruned = prune_source(source)

    assert pruned is not None
    assert pruned.startswith("def predicate(): pass")
    compile(pruned, "example.py", "exec", ast.PyCF_ONLY_AST, optimize=1)


def test_preserves_parenthesized_concatenated_docstring() -> None:
    source = (
        "def documented():\n"
        "    (\n"
        '        "first"\n'
        '        r"second"\n'
        "    )\n"
        "    removed = 1\n"
    )

    pruned = prune_source(source)

    assert pruned is not None
    tree = compile(pruned, "example.py", "exec", ast.PyCF_ONLY_AST, optimize=1)
    assert isinstance(tree, ast.Module)
    function = tree.body[0]
    assert isinstance(function, ast.FunctionDef)
    assert ast.get_docstring(function) == "firstsecond"
    assert "removed = 1" not in pruned


def test_colons_inside_signature_expressions_do_not_start_the_suite() -> None:
    source = (
        'def render(value=f"{item:>{width}}") -> lambda: int:\n'
        "    implementation = 1\n"
    )

    pruned = prune_source(source)

    assert pruned is not None
    assert "implementation" not in pruned
    compile(pruned, "example.py", "exec", ast.PyCF_ONLY_AST, optimize=1)


def test_python_314_t_string_signature() -> None:
    if sys.version_info < (3, 14):
        return
    source = 'def render(value=t"{item}"):\n    implementation = 1\n'

    pruned = prune_source(source)

    assert pruned is not None
    compile(pruned, "example.py", "exec", ast.PyCF_ONLY_AST, optimize=1)


def test_exposes_ruff_version() -> None:
    assert RUFF_VERSION == "0.16.5"
