# Vendored Ruff parser

This directory contains `ruff_python_parser` 0.0.11 from Ruff 0.16.5.

The only source changes are that `Lexer::current_range` and
`Lexer::current_flags` are public. The parser crate intentionally does not expose these lexer
details, but `prune-source` needs token byte ranges without constructing a syntax tree.

When updating Ruff, replace this directory with the new crate source, reapply those two visibility
changes, run the Rust and Python tests, and repeat the corpus-level differential check.
