"""Fuzzy-index rescue port (ADR 0049 D5)."""

from __future__ import annotations

from .score import ContentMatch, FileMatch, fuzzy_score, trigrams

__all__ = ["ContentMatch", "FileMatch", "fuzzy_score", "trigrams"]
