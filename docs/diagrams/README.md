# Amux System Diagrams

| Kind | View |
| --- | --- |
| Component/topology | [Component/topology](topology.html) |
| Main runtime sequence | [Main runtime sequence](runtime-sequence.html) |
| Data flow | [Data flow](data-flow.html) |

Open the HTML files in a browser. Each is a self-contained accessible inline SVG with embedded CSS, Diagram Design's default light palette and a `doc-wide` canvas (1280 × 720). Google Fonts supplies Instrument Serif, Geist and Geist Mono; local generic fallbacks work offline. Narrow screens scroll each figure locally; print scales it to fit.

These are engineering overviews of this checkout, not a deployment-health claim. They collapse API handlers, provider adapters, queues and database tables into their owning components. Other execution backends/providers, cloud services, detailed recovery state machines and the Local-CI kit are omitted. No planned feature is drawn as implemented.

## Sources and Boundaries

| Surface | Primary implementation and supporting docs |
| --- | --- |
| CLI, identity, groups and stamping | [Bash CLI](../../amux) (`cmd_send`), [send path](../../crates/amux-server/src/api/session_verbs.rs) (`send_post`, `cross_group_policy_ok`, `origin_stamped`), [auth](../../crates/amux-server/src/api/auth.rs) |
| Delivery choice and receipts | [Delivery resolver](../../crates/amux-server/src/api/delivery/mod.rs), [native ledger](../../crates/amux-server/src/api/delivery/ledger.rs), [native delivery contract](../native-claude-delivery.md) |
| tmux and native workers | [tmux backend](../../crates/amux-server/src/backend/tmux.rs), [backend seam](../../crates/amux-server/src/backend/mod.rs), [provider adapters](../../crates/amux-server/src/provider/mod.rs); the CLI's owner-route help names the hub pane, and tmux exit census accounts for side panes |
| Vault and grants | [Scoped secrets](../../crates/amux-server/src/api/vault_secrets.rs), [vault spending](../../crates/amux-server/src/api/vault.rs), [grants](../../crates/amux-server/src/api/grants.rs); grants do not widen shared-group worker messaging |
| Cards and signals | [Board store](../../crates/amux-server/src/db/board_store.rs), [signals](../../crates/amux-server/src/api/signals.rs); signals clear explicit waits, not arbitrary dependencies |
| Status, history and human views | [Native status](../../crates/amux-server/src/api/native_status.rs), [message capture](../../crates/amux-server/src/runtime_jobs/message_capture.rs), [messages](../../crates/amux-server/src/api/messages.rs), [dashboard](../../crates/amux-dashboard/static/app.js) |

`MAX_*` names in the data view denote owner-configured environment names from the brief. The vault accepts valid configured keys and preserves their names; it does not require or add that prefix. The audit/lead view represents reads of existing records, not a separate feed service. Secret values never belong in that view. Hub and side panes are grouped as terminal infrastructure, not additional coordination services.

## Maintenance and Checks

The shared ai-config **System diagrams** rule owns maintenance: update these diagrams in the same commit range as architecture changes. [ARCHITECTURE_PATHS](ARCHITECTURE_PATHS) lists case-sensitive Python `fnmatch` patterns over repository-relative paths (`*` includes `/`). Both root-level and the actual server migration directory are included.

Run the shared guard from the repo root after committing; it checks committed history, not working-tree edits:

```sh
python3 /Users/mwang/GitHub/ai-config/.claude/worktrees/diagram-rule/scripts/check-diagrams.py --repo . --base origin/main
```

On another machine, resolve `scripts/check-diagrams.py` from the ai-config checkout. Exit 0 with JSON `status: pass` is the expected result. Run Diagram Design's `scripts/self_check.py <file>` on each HTML file as well. The automated checks establish inventory, accessible SVG and single-file structure; source review establishes the truth of each relationship.
