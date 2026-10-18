// unstamped-kind.spec coverage — use shared browser defaults and route diagnostics.
import { test, expect } from './fixtures';

// AMUX-2670. A raw-tmux fallback send is reconciled into the trail by the CLI
// as type `raw-tmux-fallback`. It must NOT render as an ordinary human message:
// that is the card's complaint in one line — "they stay indistinguishable from
// audited sends". Its delivery was never verified (keystrokes reached a pane; a
// picker may have eaten them) and its origin is the CLI's claim, not a
// server-side stamp.

test('a reconciled raw-tmux send is not classified as an ordinary human message', async ({ page }) => {
  await page.goto('/');
  await page.waitForLoadState('networkidle');
  const r = await page.evaluate(() => {
    const w = window as any;
    return {
      fallback: w._msgKind({ type: 'raw-tmux-fallback' }),
      // CONTROLS — the other types must be untouched, or this fix would be
      // reclassifying real traffic to make one case visible.
      human: w._msgKind({ type: 'direct' }),
      empty: w._msgKind({ type: '' }),
      session: w._msgKind({ type: 'session' }),
      schedule: w._msgKind({ type: 'schedule' }),
      system: w._msgKind({ type: 'system' }),
    };
  });
  expect(r.fallback, 'must have its own kind').toBe('unstamped');
  expect(r.fallback).not.toBe('human');
  expect(r.human).toBe('human');
  expect(r.empty).toBe('human');
  expect(r.session).toBe('session');
  expect(r.schedule).toBe('schedule');
  expect(r.system).toBe('amux');
});

test('the unstamped kind is visually distinct, not just named', async ({ page }) => {
  await page.goto('/');
  await page.waitForLoadState('networkidle');
  // A kind with no style renders identically to its neighbours, which would
  // leave the classification true and the SCREEN still wrong.
  const styles = await page.evaluate(() => {
    const mk = (cls: string) => {
      const d = document.createElement('div');
      d.className = 'peek-prompt ' + cls;
      document.body.appendChild(d);
      const cs = getComputedStyle(d);
      const out = { color: cs.borderLeftColor, style: cs.borderLeftStyle };
      d.remove();
      return out;
    };
    return { unstamped: mk('peek-prompt-unstamped'), human: mk('peek-prompt-human') };
  });
  expect(styles.unstamped.color).not.toBe(styles.human.color);
  expect(styles.unstamped.style).toBe('dashed');
});
