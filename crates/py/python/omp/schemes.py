"""Extension-declared URL schemes for the frozen :mod:`omp` package (ADR 0050).

An extension claims its URL schemes in the manifest ``[[schemes]]`` table and
registers them at activation with :func:`register`. A declared spelling has one
canonical form, with no aliases: it is lowercased with ASCII-only folding, must
match ``^[a-z][a-z0-9+.-]*$`` in full, must fit :data:`MAX_SCHEME_BYTES`, and
must not be a built-in wire name of the URL vocabulary that :mod:`omp.urls`
loads (``https`` and ``pr`` included). One extension registers at most
:data:`MAX_SCHEMES` schemes; a description fits :data:`MAX_DESCRIPTION_BYTES`.

Registrations are stored inert until ADR 0050 S2: nothing here binds a
resolver, :func:`omp.urls.schemes` still returns the built-in table, and a
declared scheme still parses as ``omp.urls.Scheme.UNKNOWN``. The host checks
each registration against the manifest's claims when S2 binds it.
"""

from __future__ import annotations

import re as _re
import string as _string
import threading as _threading
from dataclasses import dataclass as _dataclass
from typing import Final as _Final

from _omp_url_vocab import SCHEMES as _SCHEMES

from .urls import UrlError as _UrlError


class SchemeError(_UrlError):
    """A URL-scheme declaration was refused."""


class SchemeGrammarError(SchemeError):
    """A spelling misses ``^[a-z][a-z0-9+.-]*$`` after ASCII lowercasing."""


class BuiltinSchemeError(SchemeError):
    """A spelling is a built-in wire name of the URL vocabulary."""


class DuplicateSchemeError(SchemeError):
    """A spelling is declared twice after normalization."""


class SchemeLimitError(SchemeError):
    """A declaration exceeds a byte limit or the per-extension count."""


MAX_SCHEME_BYTES: _Final = 64
MAX_SCHEMES: _Final = 32
MAX_DESCRIPTION_BYTES: _Final = 4 * 1024

_GRAMMAR: _Final = _re.compile(r"[a-z][a-z0-9+.-]*")
_ASCII_LOWER: _Final = str.maketrans(_string.ascii_uppercase, _string.ascii_lowercase)
_BUILTIN_WIRE_NAMES: _Final = frozenset(
    spelling for _member, wire, _selectors in _SCHEMES for spelling in wire
)
_lock: _Final = _threading.Lock()
_declared: tuple[Declaration, ...] = ()


def _canonical(spelling: object) -> str:
    if not isinstance(spelling, str):
        raise TypeError("scheme name must be str")
    if len(spelling) > MAX_SCHEME_BYTES:
        raise SchemeLimitError(
            f"URL scheme is longer than {MAX_SCHEME_BYTES} bytes"
        )
    canonical = spelling.translate(_ASCII_LOWER)
    if _GRAMMAR.fullmatch(canonical) is None:
        raise SchemeGrammarError(
            f"URL scheme {spelling!r} must match ^[a-z][a-z0-9+.-]*$"
        )
    if canonical in _BUILTIN_WIRE_NAMES:
        raise BuiltinSchemeError(f"URL scheme {canonical!r} is a built-in wire name")
    return canonical


@_dataclass(frozen=True, slots=True)
class Declaration:
    """One URL scheme an extension declares, mirroring a manifest ``[[schemes]]`` entry."""

    name: str
    description: str | None = None
    immutable: bool = False
    writable: bool = False

    def __post_init__(self) -> None:
        object.__setattr__(self, "name", _canonical(self.name))
        if self.description is not None:
            if not isinstance(self.description, str):
                raise TypeError("scheme description must be str or None")
            try:
                size = len(self.description.encode("utf-8"))
            except UnicodeEncodeError:
                raise SchemeError("scheme description must be valid UTF-8 text") from None
            if size > MAX_DESCRIPTION_BYTES:
                raise SchemeLimitError(
                    f"scheme description is {size} bytes; the limit is {MAX_DESCRIPTION_BYTES}"
                )
        for key in ("immutable", "writable"):
            if not isinstance(getattr(self, key), bool):
                raise TypeError(f"scheme {key} must be bool")


def register(*declarations: Declaration | str) -> tuple[Declaration, ...]:
    """Register URL schemes for this extension; stored inert until ADR 0050 S2.

    Each argument is a :class:`Declaration` or a bare spelling. A spelling
    repeated after normalization, in this call or an earlier one, raises
    :class:`DuplicateSchemeError`; more than :data:`MAX_SCHEMES` in total raises
    :class:`SchemeLimitError`. A refused call registers nothing.
    """
    global _declared
    if len(declarations) > MAX_SCHEMES:
        raise SchemeLimitError(
            f"{len(declarations)} URL schemes declared; the limit is {MAX_SCHEMES}"
        )
    batch = tuple(
        item if isinstance(item, Declaration) else Declaration(item)
        for item in declarations
    )
    with _lock:
        names = {declaration.name for declaration in _declared}
        for declaration in batch:
            if declaration.name in names:
                raise DuplicateSchemeError(
                    f"URL scheme {declaration.name!r} is declared twice"
                )
            names.add(declaration.name)
        if len(names) > MAX_SCHEMES:
            raise SchemeLimitError(
                f"{len(names)} URL schemes declared; the limit is {MAX_SCHEMES}"
            )
        _declared = (*_declared, *batch)
    return batch


def declared() -> tuple[Declaration, ...]:
    """Return this extension's registered declarations, in registration order."""
    return _declared


__all__ = (
    "BuiltinSchemeError",
    "Declaration",
    "DuplicateSchemeError",
    "MAX_DESCRIPTION_BYTES",
    "MAX_SCHEMES",
    "MAX_SCHEME_BYTES",
    "SchemeError",
    "SchemeGrammarError",
    "SchemeLimitError",
    "declared",
    "register",
)
