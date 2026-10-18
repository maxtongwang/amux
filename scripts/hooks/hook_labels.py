"""Reconcile managed hook handlers without moving their trust positions."""

from __future__ import annotations

from typing import Any, Callable


def reconcile(
    hooks: dict[str, Any],
    desired: dict[str, list[dict[str, Any]]],
    owns: Callable[[str], bool],
) -> int:
    """Update matching handlers in place; return the number of changed handlers."""
    pending = {(event, group.get("matcher")): group for event, groups in desired.items() for group in groups}
    changed = 0
    for event, groups in list(hooks.items()):
        if not isinstance(groups, list):
            raise ValueError(f"settings hooks.{event} must be an array")
        kept_groups = []
        for raw_group in groups:
            if not isinstance(raw_group, dict) or not isinstance(raw_group.get("hooks"), list):
                kept_groups.append(raw_group)
                continue
            kept_handlers = []
            for handler in raw_group["hooks"]:
                if not isinstance(handler, dict) or not owns(handler.get("command", "")):
                    kept_handlers.append(handler)
                    continue
                key = (event, raw_group.get("matcher"))
                template = pending.pop(key, None)
                if template is None:
                    changed += 1
                    continue
                replacement = dict(handler)
                replacement.update({k: v for k, v in template["hooks"][0].items() if k != "statusMessage"})
                replacement.setdefault("statusMessage", template["hooks"][0]["statusMessage"])
                changed += replacement != handler
                kept_handlers.append(replacement)
            if kept_handlers:
                if kept_handlers == raw_group["hooks"]:
                    kept_groups.append(raw_group)
                else:
                    kept_groups.append(dict(raw_group, hooks=kept_handlers))
        if kept_groups:
            hooks[event] = kept_groups
        else:
            hooks.pop(event, None)
    for (event, _), template in pending.items():
        hooks.setdefault(event, []).append(template)
        changed += 1
    return changed
