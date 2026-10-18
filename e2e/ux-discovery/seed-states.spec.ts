// Seed state fixture verification (RR-0028c, Invariant 46).
//
// Each fixture represents a deterministic dashboard state. The tests verify
// the crawler discovers the controls specific to that state — a control the
// crawler cannot find in a seed state is a control no acceptance test covers
// for that state.

import { test, expect } from '../fixtures';
import { crawl, captureState } from './crawler';
import * as path from 'path';

const fixture = (name: string) => 'file://' + path.join(__dirname, 'fixtures', name);

// ---------------------------------------------------------------------------
// Empty installation
// ---------------------------------------------------------------------------

test.describe('empty installation', () => {
  const FIXTURE = fixture('empty-installation.html');

  test('discovers onboarding controls', async ({ page }) => {
    await page.goto(FIXTURE);
    const state = await captureState(page, 0);
    const ids = state.actions.map((a) => a.id);

    expect(ids).toContain('add-first-worker');
    expect(ids).toContain('tab-sessions');
    expect(ids).toContain('tab-board');
    expect(ids).toContain('tab-schedules');
    expect(ids).toContain('tab-settings');
  });

  test('crawl reaches hidden panels via tab clicks', async ({ page }) => {
    const graph = await crawl(page, FIXTURE, { maxDepth: 2, maxStates: 20 });
    const allIds = graph.states.flatMap((s) => s.actions.map((a) => a.id));

    expect(allIds).toContain('create-first-task');
    expect(allIds).toContain('create-first-schedule');
    expect(allIds).toContain('save-settings');
    expect(allIds).toContain('setting-default-backend');
    expect(allIds).toContain('setting-default-provider');
  });
});

// ---------------------------------------------------------------------------
// Worker states
// ---------------------------------------------------------------------------

test.describe('worker states', () => {
  const FIXTURE = fixture('worker-states.html');

  test('discovers all worker state variants', async ({ page }) => {
    await page.goto(FIXTURE);
    const state = await captureState(page, 0);
    const ids = state.actions.map((a) => a.id);

    // Stopped: start, configure, delete
    expect(ids).toContain('worker-stopped-start');
    expect(ids).toContain('worker-stopped-configure');
    expect(ids).toContain('worker-stopped-delete');

    // Starting: cancel
    expect(ids).toContain('worker-starting-cancel');

    // Active: send, peek, stop
    expect(ids).toContain('worker-active-send');
    expect(ids).toContain('worker-active-peek');
    expect(ids).toContain('worker-active-stop');

    // Idle: send, peek, stop, restart
    expect(ids).toContain('worker-idle-send');
    expect(ids).toContain('worker-idle-restart');

    // Waiting: peek, unblock, stop
    expect(ids).toContain('worker-waiting-unblock');

    // RateLimited: peek, stop (no restart — not stalled per Invariant 10)
    expect(ids).toContain('worker-ratelimited-peek');
    expect(ids).toContain('worker-ratelimited-stop');

    // Error: retry, view-log, stop
    expect(ids).toContain('worker-error-retry');
    expect(ids).toContain('worker-error-view-log');

    // High context: compact
    expect(ids).toContain('worker-high-context-compact');

    // Unread messages: view-messages
    expect(ids).toContain('worker-unread-view-messages');
  });

  test('all controls have semantic ids', async ({ page }) => {
    await page.goto(FIXTURE);
    const state = await captureState(page, 0);
    expect(state.actions.every((a) => a.hasSemanticId)).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// Task lifecycle
// ---------------------------------------------------------------------------

test.describe('task lifecycle', () => {
  const FIXTURE = fixture('task-lifecycle.html');

  test('discovers all task status variants', async ({ page }) => {
    await page.goto(FIXTURE);
    const state = await captureState(page, 0);
    const ids = state.actions.map((a) => a.id);

    // Backlog: promote, edit, discard
    expect(ids).toContain('task-backlog-promote');
    expect(ids).toContain('task-backlog-discard');

    // Todo: assign, start, edit, discard
    expect(ids).toContain('task-todo-assign');
    expect(ids).toContain('task-todo-start');

    // Doing: done, review, block, needsyou
    expect(ids).toContain('task-doing-done');
    expect(ids).toContain('task-doing-review');
    expect(ids).toContain('task-doing-block');
    expect(ids).toContain('task-doing-needsyou');

    // Review: approve, reject
    expect(ids).toContain('task-review-approve');
    expect(ids).toContain('task-review-reject');

    // NeedsYou: respond, resume
    expect(ids).toContain('task-needsyou-respond');
    expect(ids).toContain('task-needsyou-resume');

    // Blocked: unblock
    expect(ids).toContain('task-blocked-unblock');

    // Done: verify, reopen, archive
    expect(ids).toContain('task-done-verify');
    expect(ids).toContain('task-done-reopen');

    // Verified: archive (terminal)
    expect(ids).toContain('task-verified-archive');

    // Discarded: restore, archive (terminal)
    expect(ids).toContain('task-discarded-restore');

    // Armed: fire, disarm
    expect(ids).toContain('task-armed-fire');
    expect(ids).toContain('task-armed-disarm');

    // Quarantined: inspect, archive (terminal)
    expect(ids).toContain('task-quarantined-inspect');
    expect(ids).toContain('task-quarantined-archive');
  });

  test('all controls have semantic ids', async ({ page }) => {
    await page.goto(FIXTURE);
    const state = await captureState(page, 0);
    expect(state.actions.every((a) => a.hasSemanticId)).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// Populated installation
// ---------------------------------------------------------------------------

test.describe('populated installation', () => {
  const FIXTURE = fixture('populated-installation.html');

  test('discovers group-scoped workers and board controls', async ({ page }) => {
    await page.goto(FIXTURE);
    const state = await captureState(page, 0);
    const ids = state.actions.map((a) => a.id);

    expect(ids).toContain('worker-alpha-send');
    expect(ids).toContain('worker-alpha-peek');
    expect(ids).toContain('worker-delta-start');
    expect(ids).toContain('add-worker');
    expect(ids).toContain('tab-board');
    expect(ids).toContain('tab-schedules');
  });

  test('crawl reaches board and schedule panels', async ({ page }) => {
    const graph = await crawl(page, FIXTURE, { maxDepth: 2, maxStates: 20 });
    const allIds = graph.states.flatMap((s) => s.actions.map((a) => a.id));

    expect(allIds).toContain('board-create-task');
    expect(allIds).toContain('board-filter-status');
    expect(allIds).toContain('board-search');
    expect(allIds).toContain('schedule-daily-run');
    expect(allIds).toContain('schedule-create');
  });
});

// ---------------------------------------------------------------------------
// Offline / sync
// ---------------------------------------------------------------------------

test.describe('offline and sync states', () => {
  const FIXTURE = fixture('offline-sync.html');

  test('discovers offline controls', async ({ page }) => {
    await page.goto(FIXTURE);
    const state = await captureState(page, 0);
    const ids = state.actions.map((a) => a.id);

    expect(ids).toContain('offline-retry');
    expect(ids).toContain('offline-send-queued');
    expect(ids).toContain('offline-peek-cached');
  });

  test('discovers mutation queue controls', async ({ page }) => {
    await page.goto(FIXTURE);
    const state = await captureState(page, 0);
    const ids = state.actions.map((a) => a.id);

    expect(ids).toContain('mutation-1-discard');
    expect(ids).toContain('mutation-2-discard');
    expect(ids).toContain('mutation-3-discard');
    expect(ids).toContain('mutations-flush');
    expect(ids).toContain('mutations-discard-all');
  });

  test('discovers conflict resolution controls', async ({ page }) => {
    await page.goto(FIXTURE);
    const state = await captureState(page, 0);
    const ids = state.actions.map((a) => a.id);

    expect(ids).toContain('conflict-keep-local');
    expect(ids).toContain('conflict-keep-server');
    expect(ids).toContain('conflict-merge');
    expect(ids).toContain('syncing-cancel');
  });
});

// ---------------------------------------------------------------------------
// Provider / backend / schedules / browser profiles
// ---------------------------------------------------------------------------

test.describe('provider and backend states', () => {
  const FIXTURE = fixture('provider-backend-states.html');

  test('discovers provider-unavailable controls', async ({ page }) => {
    await page.goto(FIXTURE);
    const state = await captureState(page, 0);
    const ids = state.actions.map((a) => a.id);

    expect(ids).toContain('provider-claude-reauthenticate');
    expect(ids).toContain('provider-claude-configure');
    expect(ids).toContain('provider-gemini-configure');
  });

  test('discovers herdr-unavailable controls', async ({ page }) => {
    await page.goto(FIXTURE);
    const state = await captureState(page, 0);
    const ids = state.actions.map((a) => a.id);

    expect(ids).toContain('backend-herdr-retry');
    expect(ids).toContain('backend-tmux-configure');
  });

  test('discovers schedule enable/disable controls', async ({ page }) => {
    await page.goto(FIXTURE);
    const state = await captureState(page, 0);
    const ids = state.actions.map((a) => a.id);

    expect(ids).toContain('schedule-enabled-disable');
    expect(ids).toContain('schedule-enabled-run');
    expect(ids).toContain('schedule-disabled-enable');
    expect(ids).toContain('schedule-create');
  });

  test('discovers browser profile controls', async ({ page }) => {
    await page.goto(FIXTURE);
    const state = await captureState(page, 0);
    const ids = state.actions.map((a) => a.id);

    expect(ids).toContain('profile-active-release');
    expect(ids).toContain('profile-active-screenshot');
    expect(ids).toContain('profile-locked-force-release');
    expect(ids).toContain('profile-available-start');
    expect(ids).toContain('profile-available-configure');
  });
});

// ---------------------------------------------------------------------------
// Migrated dataset
// ---------------------------------------------------------------------------

test.describe('migrated dataset', () => {
  const FIXTURE = fixture('migrated-dataset.html');

  test('discovers migration-specific controls', async ({ page }) => {
    await page.goto(FIXTURE);
    const state = await captureState(page, 0);
    const ids = state.actions.map((a) => a.id);

    expect(ids).toContain('migration-dismiss');
    expect(ids).toContain('migration-view-report');
    expect(ids).toContain('migrated-worker-rename');
    expect(ids).toContain('migrated-task-retype');
    expect(ids).toContain('migrated-task-clean-desc');
    expect(ids).toContain('migrated-task-decompose');
    expect(ids).toContain('migrated-schedule-verify');
    expect(ids).toContain('migrated-memory-review');
    expect(ids).toContain('migrated-memory-discard-stale');
  });

  test('all controls have semantic ids', async ({ page }) => {
    await page.goto(FIXTURE);
    const state = await captureState(page, 0);
    expect(state.actions.every((a) => a.hasSemanticId)).toBe(true);
  });
});
