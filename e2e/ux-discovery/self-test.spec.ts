// Crawler self-verification (RR-0028d, Invariant 46).
//
// Runs the discovery crawler against a fixture page with intentionally
// hidden, nested, hover-only, keyboard-only, and scroll-revealed controls,
// and asserts every expected control is discovered. A discovery system that
// silently misses paths creates false confidence — this suite is what makes
// "the crawler found everything" a falsifiable claim.

import { test, expect } from '../fixtures';
import { crawl, captureState } from './crawler';
import * as path from 'path';

const FIXTURE = 'file://' + path.join(__dirname, 'fixtures', 'self-test.html');

test('inventory discovers every control present in the DOM', async ({ page }) => {
  await page.goto(FIXTURE);
  const state = await captureState(page, 0);
  const ids = state.actions.map((a) => a.id);

  // Directly visible controls.
  expect(ids).toContain('visible-button');
  expect(ids).toContain('visible-link');
  expect(ids).toContain('visible-input');
  expect(ids).toContain('menu-opener');
  expect(ids).toContain('modal-opener');
  expect(ids).toContain('drawer-toggle');
  expect(ids).toContain('keyboard-shortcut-target');
  expect(ids).toContain('below-fold-button'); // scroll position must not hide inventory

  // The anonymous control is discovered AND flagged as missing a semantic id.
  const anonymous = state.actions.find((a) => !a.hasSemanticId && a.label === 'Anonymous button');
  expect(anonymous).toBeTruthy();
});

test('crawl reaches controls revealed by other controls', async ({ page }) => {
  const graph = await crawl(page, FIXTURE, { maxDepth: 3, maxStates: 30 });
  const allActionIds = graph.states.flatMap((s) => s.actions.map((a) => a.id));

  // Nested menu item, modal confirm, and drawer action only exist in states
  // reached BY clicking their openers — presence proves multi-step
  // exploration works.
  expect(allActionIds).toContain('nested-menu-item');
  expect(allActionIds).toContain('modal-confirm');
  expect(allActionIds).toContain('drawer-action');

  // The graph records edges, not just states.
  expect(graph.edges.length).toBeGreaterThan(0);

  // The anonymous control lands in the CI-failure list.
  expect(graph.missingSemanticIds.some((a) => a.label === 'Anonymous button')).toBe(true);
});

test('semantic hashing deduplicates volatile content', async ({ page }) => {
  await page.goto(FIXTURE);
  const s1 = await captureState(page, 0);
  // Mutate ONLY volatile content: a timestamp and a counter.
  await page.evaluate(() => {
    const el = document.getElementById('result')!;
    el.textContent = 'updated 2026-08-09T12:00:00Z — 42 items';
  });
  const s2 = await captureState(page, 0);
  await page.evaluate(() => {
    const el = document.getElementById('result')!;
    el.textContent = 'updated 2026-08-09T18:30:00Z — 7 items';
  });
  const s3 = await captureState(page, 0);
  // Different timestamps/counters, same semantic state.
  expect(s2.hash).toBe(s3.hash);
  // But a structural change is a new state.
  await page.click('[data-testid="menu-opener"]');
  const s4 = await captureState(page, 0);
  expect(s4.hash).not.toBe(s1.hash);
});
