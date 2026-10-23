#!/usr/bin/env python3
"""Append one best-effort safe-cargo slot event without recording command arguments."""

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("event", choices=("slot_wait_start", "slot_acquired", "slot_released"))
    parser.add_argument("--worktree", required=True)
    parser.add_argument("--subcommand", required=True)
    parser.add_argument("--pid", required=True, type=int)
    parser.add_argument("--run-id", required=True)
    args = parser.parse_args()
    # Only a cargo verb is retained; option values and test/program args can contain secrets.
    verb = args.subcommand if re.fullmatch(r"[a-z][a-z0-9_-]{0,63}", args.subcommand) else "unknown"
    row = dict(event=args.event, worktree=args.worktree, command="cargo " + verb,
               subcommand=verb, pid=args.pid, run_id=args.run_id,
               ts=datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z"))
    try:
        directory = Path(os.environ.get("AMUX_HOME") or Path.home() / ".amux") / "logs"
        directory.mkdir(parents=True, exist_ok=True)
        data = (json.dumps(row, separators=(",", ":")) + "\n").encode()
        fd = os.open(directory / "build-slots.jsonl", os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
        try:
            if os.write(fd, data) != len(data):
                raise OSError("short append")
        finally:
            os.close(fd)
    except OSError:
        print("safe-cargo: warning: slot timing append failed; cargo continues", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
