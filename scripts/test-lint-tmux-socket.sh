#!/usr/bin/env bash
# scripts/lint-tmux-socket.py must FAIL on a destructive tmux call without an
# explicit socket and PASS the sanctioned shapes. A lint that never fires is no
# lint: the 2026-10-16 outage was `TMUX_TMPDIR=... tmux kill-server` inside a
# pane, and that exact line is the first positive case here.
#
# Usage: scripts/test-lint-tmux-socket.sh   Exit 0 = all pass, 1 = a failure.
#
# tmux-lint: allow-file(lint fixtures written to a temp dir as data; nothing here runs tmux)
set -euo pipefail
cd "$(dirname "$0")/.."
LINT=scripts/lint-tmux-socket.py
PASS=0; FAIL=0
check() { if [ "$2" = "$3" ]; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); echo "  FAIL  $1 (want $2, got $3)"; fi; }
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT

# ── Positive: each must be flagged ──────────────────────────────────────────
cat > "$T/bad.sh" <<'EOF'
TMUX_TMPDIR="$dir" tmux kill-server
tmux kill-session -t =probe
env -u TMUX tmux send-keys -t =probe Enter
EOF
cat > "$T/bad.py" <<'EOF'
subprocess.run(["tmux", "kill-server"], env=env)
subprocess.run(['tmux',
                'send-keys', '-t', name, 'Enter'])
EOF
cat > "$T/bad.mjs" <<'EOF'
execFileSync('tmux', ['kill-session', '-t', 'x'], { env });
EOF
cat > "$T/empty-reason.sh" <<'EOF'
tmux kill-server  # tmux-lint: allow()
EOF
rc=0
out=$("$LINT" "$T/bad.sh" "$T/bad.py" "$T/bad.mjs" "$T/empty-reason.sh" 2>/dev/null) || rc=$?
check "the lint fails on unsocketed destructive calls" 1 "$rc"
check "every positive line is reported (3 sh + 2 py + 1 mjs + 1 empty allow)" 7 "$(printf '%s\n' "$out" | grep -c .)"
rc=0; printf '%s\n' "$out" | grep -q 'bad.sh:1: tmux kill-server' || rc=$?; check "the outage line itself is named" 0 "$rc"
rc=0; printf '%s\n' "$out" | grep -q 'bad.py:3: tmux send-keys' || rc=$?; check "a quoted verb on a continued argv line is caught" 0 "$rc"
rc=0; printf '%s\n' "$out" | grep -q 'allow() needs a reason' || rc=$?; check "an allow without a reason is an error" 0 "$rc"

# ── Negative: each must pass ────────────────────────────────────────────────
cat > "$T/good.sh" <<'EOF'
env -u TMUX -u TMUX_PANE tmux -S "$sock" kill-server
tmux -L amux-test-123 kill-session -t =probe
amux_tmux send-keys -t =probe Enter
# a comment describing `tmux kill-server` is not a call
/usr/bin/tmux kill-session -t amux-init  # tmux-lint: allow(production boot script, never run by a test)
EOF
cat > "$T/good.py" <<'EOF'
subprocess.run(['tmux', '-S', sock, 'kill-server'], env=env)
subprocess.run(tmux_argv(env, 'send-keys', '-t', name, 'Enter'), env=env)
# tmux-lint: allow(this wrapper adds -S itself)
tmux("kill-session", "-t", name)
EOF
cat > "$T/good.rs" <<'EOF'
let _ = amux_server::tmux_guard::std_tmux().args(["kill-server"]).status();
let _ = Command::new("tmux").arg("-S").arg(&sock).args(["send-keys", "-t", st, "Enter"]).status();
EOF
cat > "$T/fixtures.rs" <<'EOF'
//! tmux-lint: allow-file(source-audit fixtures in string literals)
let planted = r#"tmux(&["send-keys", "-t", "amux-amux", "Enter"])"#;
EOF
rc=0; "$LINT" "$T/good.sh" "$T/good.py" "$T/good.rs" "$T/fixtures.rs" >/dev/null 2>&1 || rc=$?
check "the lint passes explicit -S/-L, the shared helpers, comments and reasoned allows" 0 "$rc"

# ── The repository itself is clean ─────────────────────────────────────────
rc=0; "$LINT" >/dev/null 2>&1 || rc=$?
check "the tracked test and tool scripts pass the lint" 0 "$rc"

echo "tmux socket lint tests: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
