// scheduler-audit.spec coverage — use shared browser defaults and route diagnostics.
import { test, expect } from './fixtures';

// AMUX-2755. GET /api/schedules/audit was routed and the SPA referenced it zero
// times. The trail existed and reached nobody — in the subsystem whose own
// incident (AMUX-1812: eight schedules vanished with no attribution) is why it
// was built.
//
// The assertions below are about the DELETED case specifically. A per-schedule
// row cannot show a deleted schedule — the row is gone — so a panel that only
// annotated live rows would reproduce the original blindness. Measured on the
// live board: 151 schedules have audit rows, 118 still exist, 45 are visible
// only here.

test('the change-history panel renders a DELETED schedule, which no row can show', async ({ page }) => {
  await page.goto('/');
  await page.waitForLoadState('networkidle');

  const r = await page.evaluate(() => {
    const w = window as any;
    // Two entries: one for a live schedule, one for a schedule that no longer
    // exists. The second is the case this panel exists for.
    const rows = [
      { schedule_id: 'SCHED-1', title: 'still here', field: 'enabled', by_who: 'amux',
        source: 'api-patch', old_value: '0', new_value: '1', ts: Math.floor(Date.now() / 1000) - 60 },
      { schedule_id: 'SCHED-GONE', title: 'vanished one', field: 'deleted', by_who: '',
        source: 'api-delete', old_value: '', new_value: '1', ts: Math.floor(Date.now() / 1000) - 120 },
    ];
    // Passed as ARGUMENTS: top-level `let`s are not window properties, so
    // assigning window._schedulerAudit would leave this function reading an
    // empty array while every assertion below still ran.
    w.renderSchedulerAudit(rows, new Set(['SCHED-1']));
    const el = document.getElementById('scheduler-audit');
    return { html: el ? el.innerHTML : '', count: document.getElementById('sched-audit-n')?.textContent || '' };
  });

  expect(r.html, 'the live schedule change must render').toContain('still here');
  expect(r.html, 'a DELETED schedule must render — no row can show it').toContain('vanished one');
  expect(r.html, 'and be marked as gone').toContain('gone');
  // An unattributed write is the exact AMUX-1812 failure; it must not render blank.
  expect(r.html, 'an unattributed write must be named, not blank').toContain('unattributed');
  expect(r.html, 'the attributed one must name its author').toContain('amux');
  expect(r.count).toBe('(2)');
});

test('an empty trail says so rather than rendering nothing', async ({ page }) => {
  await page.goto('/');
  await page.waitForLoadState('networkidle');
  const html = await page.evaluate(() => {
    const w = window as any;
    w.renderSchedulerAudit([], new Set());
    return document.getElementById('scheduler-audit')?.innerHTML || '';
  });
  expect(html).toContain('No recorded schedule changes');
});
