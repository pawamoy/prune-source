"""Prune function implementations before compiling Python abstract syntax trees."""

from __future__ import annotations

from prune_source._native import RUFF_VERSION, prune_source

__all__ = ["RUFF_VERSION", "prune_source"]
