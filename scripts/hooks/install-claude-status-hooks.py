#!/usr/bin/env python3
"""Idempotently wire amux's canonical Claude or Codex lifecycle and read-routing hooks.

--provider codex writes Codex's own wiring into ~/.codex/hooks.json (Codex 0.163, measured
2026-10-16, ai-config #131): no Notification event exists there, so `blocked` comes from the
passive observer's PermissionRequest instead; Codex has no Read tool, so the read router is wired
for shell commands (`Bash`) only; and the ask guard is not wired, because the owner-ask goal check
reads Claude transcripts only. Codex runs a new or changed user hook only after it is trusted in
its `/hooks` review.

The status-report hook (hook-report.sh) is retired: no report entry is registered for either
provider, and every older managed report command is removed on each run. Unrelated settings and
hooks are preserved. The large-read router
is installed once for both Read and Bash. Re-running install cannot multiply
hooks or leave an inline fork active beside the real scripts.
"""

from __future__ import annotations

import argparse
import json
import os
import stat
import tempfile
from pathlib import Path
from typing import Any

import sys as _sys
# The shared labeller lives beside this file; importlib loaders (tests) and runs from any cwd must find it.
_sys.path.insert(0, str(__import__('pathlib').Path(__file__).resolve().parent))
from hook_labels import reconcile  # noqa: E402


REPORT_MARKERS = ("hook-report.sh", "amux-report.sh")
READ_GUARD_MARKERS = ("large-read-guard.py",)
ASK_GUARD_MARKERS = ("ask-guard.py",)


def is_amux_report(command: Any) -> bool:
    if not isinstance(command, str):
        return False
    return any(marker in command for marker in REPORT_MARKERS) or (
        "/api/sessions/" in command and "/report" in command
    )


def is_amux_read_guard(command: Any) -> bool:
    return isinstance(command, str) and any(marker in command for marker in READ_GUARD_MARKERS)


def is_amux_ask_guard(command: Any) -> bool:
    return isinstance(command, str) and any(marker in command for marker in ASK_GUARD_MARKERS)


def group(command: str, matcher: str | None = None) -> dict[str, Any]:
    out: dict[str, Any] = {
        "hooks": [{"type": "command", "command": command, "timeout": 10}]
    }
    if matcher is not None:
        out["matcher"] = matcher
    return out


def canonical(hook_path: str) -> dict[str, dict[str, Any]]:
    """Retired: amux no longer registers a status-report hook for Claude. Kept so callers and
    tests can assert the desired set is empty; merge() still removes older managed entries."""
    return {}


def canonical_codex(hook_path: str) -> dict[str, dict[str, Any]]:
    """Retired for Codex as well; see canonical()."""
    return {}


def canonical_read_guard(hook_path: str, provider: str = "claude") -> list[dict[str, Any]]:
    quoted = '"' + hook_path.replace('"', '\\"') + '"'
    command = f"python3 {quoted}"
    if provider == "codex":
        router = group(command, "Bash")  # Codex reads files through its shell tool
        router["hooks"][0]["statusMessage"] = "amux read router"
        return [router]
    return [group(command, "Read"), group(command, "Bash")]


def canonical_ask_guard(hook_path: str) -> list[dict[str, Any]]:
    # AMUX-5234: AskUserQuestion under an active /goal becomes a needsyou card.
    # ANCHORED, for the reason the subagent matcher in hook-report.sh gives:
    # the matcher is an unanchored regex over the tool name.
    quoted = '"' + hook_path.replace('"', '\\"') + '"'
    return [group(f"python3 {quoted}", "^AskUserQuestion$")]


def merge(
    data: dict[str, Any],
    hook_path: str,
    read_guard_path: str = "$HOME/.amux/hooks/large-read-guard.py",
    ask_guard_path: str | None = None,
    provider: str = "claude",
    changes: list[int] | None = None,
) -> dict[str, Any]:
    if provider not in ("claude", "codex"):
        raise ValueError(f"unknown provider {provider!r}")
    if provider == "codex" and ask_guard_path is not None:
        raise ValueError("the ask guard has no Codex wiring: owner-ask goal detection reads Claude transcripts only")
    raw_hooks = data.setdefault("hooks", {})
    if not isinstance(raw_hooks, dict):
        raise ValueError("settings 'hooks' must be an object")

    if provider == "codex":
        report_groups = {event: [event_group] for event, event_group in canonical_codex(hook_path).items()}
        count = reconcile(raw_hooks, report_groups, lambda command: is_amux_report(command))
        count += reconcile(raw_hooks, {"PreToolUse": canonical_read_guard(read_guard_path, provider)}, is_amux_read_guard)
        if changes is not None:
            changes.append(count)
        return data

    # Remove only managed amux commands. A group can contain unrelated commands
    # beside one old hook; keep the group and every unrelated hook intact.
    for event, groups in list(raw_hooks.items()):
        if not isinstance(groups, list):
            raise ValueError(f"settings hooks.{event} must be an array")
        kept_groups = []
        for raw_group in groups:
            if not isinstance(raw_group, dict):
                kept_groups.append(raw_group)
                continue
            commands = raw_group.get("hooks")
            if not isinstance(commands, list):
                kept_groups.append(raw_group)
                continue
            kept_commands = [
                item
                for item in commands
                if not (
                    isinstance(item, dict)
                    and (
                        is_amux_report(item.get("command"))
                        or is_amux_read_guard(item.get("command"))
                        or (ask_guard_path is not None and is_amux_ask_guard(item.get("command")))
                    )
                )
            ]
            if kept_commands:
                next_group = dict(raw_group)
                next_group["hooks"] = kept_commands
                kept_groups.append(next_group)
        if kept_groups:
            raw_hooks[event] = kept_groups
        else:
            raw_hooks.pop(event, None)

    for event, report_group in canonical(hook_path).items():
        raw_hooks.setdefault(event, []).append(report_group)
    raw_hooks.setdefault("PreToolUse", []).extend(canonical_read_guard(read_guard_path, provider))
    if ask_guard_path is not None:
        raw_hooks["PreToolUse"].extend(canonical_ask_guard(ask_guard_path))
    return data


def write_atomic(path: Path, data: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    mode = stat.S_IMODE(path.stat().st_mode) if path.exists() else 0o600
    encoded = (json.dumps(data, indent=2, ensure_ascii=False) + "\n").encode()
    fd, temp_name = tempfile.mkstemp(prefix=path.name + ".", dir=path.parent)
    try:
        os.fchmod(fd, mode)
        with os.fdopen(fd, "wb") as stream:
            stream.write(encoded)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temp_name, path)
    except BaseException:
        try:
            os.unlink(temp_name)
        except FileNotFoundError:
            pass
        raise


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--settings",
        type=Path,
        default=None,
        help="native hook file (default: ~/.claude/settings.json, or $CODEX_HOME/hooks.json for --provider codex)",
    )
    parser.add_argument("--hook-path", default="$HOME/.amux/hook-report.sh", help="accepted for backward compatibility; no effect")
    parser.add_argument(
        "--read-guard-path",
        default="$HOME/.amux/hooks/large-read-guard.py",
    )
    parser.add_argument(
        "--ask-guard-path",
        default=None,
        help="wire the AskUserQuestion goal guard (AMUX-5234); omitted = leave any existing wiring alone",
    )
    parser.add_argument(
        "--provider",
        choices=("claude", "codex"),
        default="claude",
        help="whose native events to write (codex: ~/.codex/hooks.json)",
    )
    parser.add_argument(
        "--allow-unsupported-events",
        action="store_true",
        help="accepted for backward compatibility; no effect (the status-report hook is retired)",
    )
    parser.add_argument("--dry-run", action="store_true", help="count changed managed hooks without writing settings")
    args = parser.parse_args()
    if args.dry_run and args.provider != "codex":
        parser.error("--dry-run currently requires --provider codex")
    if args.settings is None:
        # Resolved after --provider: a Codex run defaulting to Claude's settings would replace Claude's
        # managed report hooks with the Codex set and drop its Notification producer (PR #6 review).
        if args.provider == "codex":
            args.settings = Path(os.environ.get("CODEX_HOME") or Path.home() / ".codex") / "hooks.json"
        else:
            args.settings = Path.home() / ".claude" / "settings.json"

    if args.settings.exists():
        try:
            data = json.loads(args.settings.read_text())
        except json.JSONDecodeError as exc:
            raise SystemExit(f"refusing to overwrite invalid JSON in {args.settings}: {exc}")
        if not isinstance(data, dict):
            raise SystemExit(f"refusing to overwrite non-object settings in {args.settings}")
    else:
        data = {}
    before = json.loads(json.dumps(data))
    changes: list[int] = []
    try:
        merged = merge(data, args.hook_path, args.read_guard_path, args.ask_guard_path, args.provider, changes)
    except ValueError as exc:
        raise SystemExit(f"refusing to rewrite {args.settings}: {exc}")
    if args.dry_run:
        print(f"{sum(changes)} hooks would change in {args.settings}")
        return 0
    if merged != before:
        write_atomic(args.settings, merged)
    print(f"wired amux {args.provider} read-routing hooks and removed the retired status-report hooks in {args.settings}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
