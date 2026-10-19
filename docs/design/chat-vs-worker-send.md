# Chat tab vs worker terminal: how a sent message is handled

AMUX-5356, under AMUX-5353 (Ethan, 2026-09-30: "if I send a message via chat, it
doesn't go away like worker... test all the code worker in variance and compare
to chat"). Audited against `origin/main` at 457dc4a9 plus the fix in this commit.

## One send path

Both composers call `sendPeekCmd()` in `crates/amux-dashboard/static/app.js`. The
only input that differs is the target:

- Terminal tab: `session = peekSession` (the worker).
- Chat tab: `session = _peekChatTarget()`, the `<worker>@chat` companion.

From there both go through the same steps:

1. The draft is saved to the local outbox before the composer clears
   (`_draftSave`), so a refused send is never lost.
2. `_composerPendingSends` blocks a second press while the first is in flight.
3. `doSend()` POSTs `/api/sessions/<target>/send` with `record_history: true` and
   a `msg_id`. The server records the message in history for both.
4. On success, `cmdHistoryAdd()` and `_composerAcceptLocal()` run, then the
   composer clears.

## Differences, and whether each is intended

| # | Difference | Intended? | Where |
|---|---|---|---|
| 1 | Chat sends go to the companion, never the worker's terminal. | Yes. Chat is read-only Q&A and must not interfere with the worker. | `sendPeekCmd`, `_peekChatTarget` |
| 2 | Queue mode never applies to chat. A chat send is always immediate. | Yes. The companion queues turns itself and has no steering queue. | `queued = ... && !_chatCompanionOf(session)` |
| 3 | Worker sends get a `[HH:MM]` prefix. Chat sends do not. | Yes. A chat bubble shows its own time. | `doSend`, `chatTarget` |
| 4 | The composer cleared for a worker send but stayed full for a chat send. | No. Fixed in 8e6eeffa: the draft bookkeeping was keyed on the send target (the companion), so it never reached the worker's composer. Chat now clears it explicitly. | `sendPeekCmd`, `session !== peekSession` branch |
| 5 | A chat send could show twice: the server's recorded copy plus the outbox's pending copy. | No. Fixed in 6ee037b7: the chat view reconciles pending copies by `msg_id`, never by text. | `_chatRender`, `.chat-pending` |
| 6 | Attachments sent from the Chat tab stayed in the composer and could be sent again. | No. Fixed in this commit, the same keying mistake as #4: the cleanup compared the open worker with the send target. It now uses the worker that owned the composer when the send started. | `sendPeekCmd`, `composerOwner` |

## Verified behaviour

- Live, 2026-10-02 13:36Z, iPhone 15 WebKit against the production dashboard
  (AMUX-5355): a Chat-tab send cleared the composer, rendered once with no
  stamp, got its reply, and was still present exactly once after a reload.
- `e2e/send-queue-and-chat-dup.spec.ts` covers #5 and #6. Each was
  mutation-checked: reverting the fix fails the named test. #2 has no
  dedicated test yet.

## Still open under AMUX-5353

The parent request also asked to evaluate the amux-meta-helper chat "having
issues" in the browser. That needs its own pass against that worker's Chat tab.
This document covers only the send-handling comparison.

## Connection sender attribution

The historical no-stamp Chat behaviour above is superseded by
[Message connection provenance](message-provenance.md): Chat-tab input carries
the same connection-derived Sender as worker input. An absent caller header no
longer creates an owner turn, and unverified claims stay visible in the stamp.
