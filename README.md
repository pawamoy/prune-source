# prune-source

`prune-source` is a small Rust/Ruff accelerator for static API analysis. It lexes Python source and
removes function implementation suites that an API visitor does not need, allowing CPython to
construct and walk a much smaller abstract syntax tree. It does not build an intermediate Ruff AST.

```bash
pip install prune-source
```

```python
import ast

from prune_source import prune_source

source = "def answer() -> int:\n    return 42\n"
pruned = prune_source(source)
tree = compile(
    source if pruned is None else pruned,
    "example.py",
    "exec",
    ast.PyCF_ONLY_AST,
    optimize=1,
)
```

The pruner preserves function signatures, decorators, docstrings, class bodies, and class
`__init__` implementations. Other function suites are replaced by `pass` while physical line
layout is retained. Removed multiline content is collapsed to empty lines rather than padded with
spaces; for indented suites, the `pass` remains on the original final implementation line so
definition end-line metadata stays stable. Nested function suites inside `__init__` are pruned as
well.

The input is expected to be valid for the Python interpreter that will compile it. The lexer does
not perform grammar or version validation. `prune_source` returns `None` when there is nothing to
prune; callers should compile their original source in that case.

Ruff's internal parser crate is vendored because its public lexer API does not expose token ranges.
The vendored copy has exactly two source changes, making the range and flag accessors public; see
`vendor/ruff_python_parser/UPSTREAM.md`.

Building from source requires Rust 1.96 or newer.
