#!/usr/bin/env python3
"""AMUX-4203: reproduce a live server's socket takeover on a private socket.

macOS returns ECONNREFUSED when an AF_UNIX listen queue fills. Never target the
operator's server: every tmux command specifies a temporary -S socket, and only
the fixture's independently queried PID receives signals.
"""
import errno
import json
import os
import signal
import socket
import subprocess
import sys
import tempfile


def main():
    if sys.platform != "darwin":
        print(json.dumps({"measured": False, "n_considered": 0,
                          "why_unmeasured": "this reproduction tests macOS AF_UNIX backlog behavior"}))
        return
    with tempfile.TemporaryDirectory(prefix="amux-backlog-", dir="/tmp") as directory:
        path = directory + "/socket"
        connections = []
        original_pid = None

        # Explicit -S AND no inherited TMUX/TMUX_PANE: the shared rule for every
        # tmux call a test makes (scripts/lib/test-isolation.sh).
        clean = {k: v for k, v in os.environ.items() if k not in ("TMUX", "TMUX_PANE")}

        def tmux(*args):
            return subprocess.run(["tmux", "-f", "/dev/null", "-S", path, *args],
                                  capture_output=True, text=True, timeout=5, env=clean)

        try:
            created = tmux("new-session", "-d", "-s", "original-proof", "/bin/sh")
            assert created.returncode == 0, created.stderr
            original_pid = int(tmux("-N", "display-message", "-p", "#{pid}").stdout.strip())
            before = os.stat(path).st_ino
            os.kill(original_pid, signal.SIGSTOP)
            failure = None
            for _ in range(300):
                connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                connection.settimeout(0.2)
                try:
                    connection.connect(path)
                    connections.append(connection)
                except OSError as error:
                    failure = error.errno
                    connection.close()
                    break
            assert failure == errno.ECONNREFUSED, (failure, len(connections))
            guarded = tmux("-N", "new-session", "-d", "-s", "guarded-proof", "/bin/sh")
            guarded_preserved = os.stat(path).st_ino == before
            assert guarded.returncode != 0 and guarded_preserved

            # Negative control: the unguarded primitive must recreate the
            # incident, or this fixture has not tested the dangerous condition.
            unguarded = tmux("new-session", "-d", "-s", "replacement-proof", "/bin/sh")
            replaced = os.stat(path).st_ino != before
            replacement_pid = int(tmux("-N", "display-message", "-p", "#{pid}").stdout.strip())
            os.kill(original_pid, 0)
            assert unguarded.returncode == 0 and replaced and replacement_pid != original_pid
            print(json.dumps({"measured": True, "n_considered": 2, "verdict": "PASS",
                              "connections_before_refusal": len(connections),
                              "connect_error": errno.errorcode[failure],
                              "guarded_preserved_socket": guarded_preserved,
                              "unguarded_replaced_socket": replaced,
                              "original_server_still_alive": True}))
        finally:
            for connection in connections:
                connection.close()
            if original_pid:
                for sig in (signal.SIGCONT, signal.SIGTERM):
                    try:
                        os.kill(original_pid, sig)
                    except ProcessLookupError:
                        pass
            # tmux-lint: allow(this file's tmux() wrapper adds -f /dev/null -S <temp socket> to every call)
            tmux("-N", "kill-server")


if __name__ == "__main__":
    main()
