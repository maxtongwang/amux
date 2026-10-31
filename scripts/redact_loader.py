"""Load a configured private redactor; unavailable or broken modules fail closed."""
import contextlib
import importlib.machinery
import importlib.util
import io
import os
import sys
from pathlib import Path

WITHHELD = "[excerpt withheld: redaction table unavailable]"


def _load_redact():
    configured = os.environ.get("AMUX_REDACT_MODULE", "")
    if not configured or not os.path.isabs(configured):
        return None
    try:
        path = Path(configured).resolve(strict=True)
        if not path.is_file():
            return None
        loader = importlib.machinery.SourceFileLoader("_amux_shared_redaction", str(path))
        # Never forward diagnostics or write caches into the dependency directory.
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            previous = sys.dont_write_bytecode
            try:
                sys.dont_write_bytecode = True
                module = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
                loader.exec_module(module)
                candidate = getattr(module, "redact", None)
                return candidate if callable(candidate) and isinstance(candidate(""), str) else None
            finally:
                sys.dont_write_bytecode = previous
    except (Exception, SystemExit):
        return None


_REDACT = _load_redact()


def redact(text):
    """Mask a complete excerpt before any display truncation or serialization."""
    if _REDACT is None:
        return WITHHELD
    try:
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            result = _REDACT(text)
        return result if isinstance(result, str) else WITHHELD
    except (Exception, SystemExit):
        return WITHHELD


def redact_tree(value):
    """Redact all strings in excerpt/evidence containers, retaining counts."""
    if isinstance(value, str):
        return redact(value)
    if isinstance(value, dict):
        return {key: redact_tree(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [redact_tree(item) for item in value]
    return value


def redact_event(event):
    """Keep diagnostic labels/counts readable while protecting attached text."""
    return {**{key: (value if key in {"event", "reason"} else redact_tree(value))
               for key, value in event.items()},
            "redaction_table_available": _REDACT is not None}
