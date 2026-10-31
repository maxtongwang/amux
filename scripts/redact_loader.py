"""Runtime bridge to ai-config/amux/bin/amux-origin, the sole redaction owner.

SourceFileLoader supports the extensionless source. Its CLI is protected by
``if __name__ == '__main__'``; importing under a private name does not run it.
No patterns are maintained here. Unavailable or broken redaction fails closed.
"""
import contextlib
import importlib.machinery
import importlib.util
import io
import os
from pathlib import Path

WITHHELD = "[excerpt withheld: redaction table unavailable]"


def _load_redact():
    path = Path(os.environ.get("AI_CONFIG_DIR", "~/GitHub/ai-config")).expanduser()
    loader = importlib.machinery.SourceFileLoader(
        "_amux_shared_redaction", str(path / "amux/bin/amux-origin"))
    try:
        # Never forward diagnostics from a failed dependency import.
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            module = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
            loader.exec_module(module)
        return module.redact if callable(module.redact) else None
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
