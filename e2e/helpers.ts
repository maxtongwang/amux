// Shared e2e helpers: the one actionability contract every harness click goes through, and engine-aware permissions.
// Hosted e2e failure investigation, "Proposed bounded fix" item 2 (overlay interception, F05/F08/F09/F13/F16).
import { test, BrowserContext, Locator } from '@playwright/test';
import {
  ActionableTimeout,
  clickWhenActionableWith,
  createDeadline,
  waitForActionableWith,
} from './actionable.mjs';

export { ActionableTimeout };

export interface ActionableOptions {
  /** The ONE total budget for the whole preflight (and, for clickWhenActionable, the click). Default 15000. */
  budgetMs?: number;
  /** Scroll the target into view first. false for reachability-without-scroll checks: off-screen then fails. */
  scroll?: boolean;
  /** Human name for the target in the failure message. */
  label?: string;
  /** Consecutive animation-frame samples that must agree. Default 3. */
  stableFrames?: number;
}

type Rect = { x: number; y: number; width: number; height: number };
type Viewport = { width: number; height: number };
type CoverNode = { tag: string; id: string; classes: string[] };
type Deadline = ReturnType<typeof createDeadline>;

export interface Actionable {
  deadline: Deadline;
  rect: Rect;
}

// Browser-side probe for the failure report. Self-contained: Playwright serializes it into the page.
function probeCover(el: Element) {
  const r = el.getBoundingClientRect();
  const rect = { x: r.x, y: r.y, width: r.width, height: r.height };
  const viewport = { width: innerWidth, height: innerHeight };
  const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
  if (!hit) return { rect, viewport, cover: null, why: 'the target centre is outside the viewport; nothing to hit-test' };
  if (hit === el || el.contains(hit)) return { rect, viewport, cover: null, why: null };
  const chain: CoverNode[] = [];
  let n: Element | null = hit;
  // The path stops below <body> (every element is under it, so it says
  // nothing), unless <body> itself is what covers the target.
  const top = (x: Element | null) => !x || x === document.documentElement || (x === document.body && chain.length > 0);
  while (!top(n) && chain.length < 8) {
    chain.unshift({ tag: n!.tagName.toLowerCase(), id: n!.id || '', classes: Array.from(n!.classList) });
    n = n!.parentElement;
  }
  return { rect, viewport, cover: { chain, truncated: !top(n) }, why: null };
}

/**
 * Wait until `target` can really take a click, under ONE total deadline.
 *
 * Each attempt re-resolves the locator (the board re-renders underneath us;
 * a stale handle is how the first clickSnapped broke desktop), then requires:
 * exactly one attached match, visible, enabled, optionally scrolled into view,
 * the same bounding box across consecutive animation frames with its centre in
 * the viewport, and finally Playwright's trial click, which is its own
 * hit-test: it fails while another element would receive the pointer events.
 *
 * Only this preflight is retried. Nothing here clicks, dismisses, hides or
 * forces anything: an overlay over the target is a finding, not an obstacle.
 * A passing preflight is not atomic with a later click; a re-render can still
 * race it, which is why the caller asserts the click's effect separately.
 *
 * On failure it throws ActionableTimeout naming the phase, the covering
 * element, rect, viewport, project/shard/worker, elapsed/budget, a screenshot
 * attachment and Playwright's call log, and prints one measured
 * `e2e_not_actionable` verdict line for the CI log sweep.
 *
 * The loop itself is actionable.mjs waitForActionableWith, so node:test pins
 * it with fake locators; this file adds only the browser failure context.
 */
export async function waitForActionable(target: Locator, options?: ActionableOptions): Promise<Actionable> {
  return waitForActionableWith(target, options, BROWSER_ENV);
}

/**
 * waitForActionable, then exactly ONE real click with the remaining budget.
 * The click is never retried: it may already have been delivered, and a second
 * delivery of a business action is a different bug. Assert its effect after.
 */
export async function clickWhenActionable(target: Locator, options?: ActionableOptions): Promise<void> {
  await clickWhenActionableWith(target, options, BROWSER_ENV);
}

function firstLine(e: unknown): string {
  return String((e as Error)?.message ?? e).split('\n')[0];
}

/** What a failure report needs that only a live browser and the test runner can supply. */
async function failureContext(target: Locator, label: string | null) {
  const page = target.page();
  let info: ReturnType<typeof test.info> | null = null;
  try { info = test.info(); } catch { info = null; }
  const shardCfg = info?.config.shard;
  let rect: Rect | null = null;
  let viewport: Viewport | null = page.viewportSize();
  let cover: { chain: CoverNode[]; truncated: boolean } | null = null;
  let coverWhy: string | null = null;
  if (page.isClosed()) {
    coverWhy = 'page closed';
  } else if ((await target.count().catch(() => 0)) === 0) {
    coverWhy = 'no element to probe';
  } else {
    try {
      const p = await target.first().evaluate(probeCover, undefined, { timeout: 2_000 });
      ({ rect, viewport, cover } = p);
      coverWhy = p.why;
    } catch (e) {
      coverWhy = `probe failed: ${firstLine(e)}`;
    }
  }
  let screenshot: string | null = null;
  if (info && !page.isClosed()) {
    try {
      const name = `not-actionable-${(label ?? 'target').replace(/[^\w.-]+/g, '-')}.png`;
      await info.attach(name, { body: await page.screenshot({ timeout: 5_000 }), contentType: 'image/png' });
      screenshot = name;
    } catch (e) {
      screenshot = `failed: ${firstLine(e)}`;
    }
  }
  return {
    project: info?.project.name ?? null,
    shard: process.env.AMUX_E2E_SHARD || (shardCfg ? `${shardCfg.current}/${shardCfg.total}` : null),
    worker: info ? info.workerIndex : null,
    rect,
    viewport,
    cover,
    coverWhy,
    screenshot,
  };
}

const BROWSER_ENV = { failureContext };

/**
 * Click something inside a scroll-snap container (the board kanban) reliably.
 *
 * WHY THIS EXISTS (AF-47, measured 2026-08-13). `.board-columns` is
 * `scroll-snap-type: x proximity` and each `.board-col` is `flex: 0 0 86vw`, so
 * at phone width the "done" column starts around x=1000. Playwright's click
 * scrolls the target into view and then clicks its centre — but under WebKit the
 * snap animation is STILL RUNNING when those coordinates are computed, so the
 * click lands where the button used to be. Measured: the handler was never
 * invoked (a wrapper counted 0 calls) and no request was issued at all, while
 * the same code on Chromium at 375px invoked it and got a 200.
 *
 * The damage was not the flake, it was the DIAGNOSIS it induced. The test failed
 * on "the card is still on the board", which reads as `clearDone()` being broken
 * on iOS. It cost a board card filed against the product — claiming the button
 * was unreachable at phone width — when the layout was correct by design and the
 * only broken thing was the click. Scroll first, let the snap settle, then click.
 *
 * It is now a thin name over clickWhenActionable: the snap settling is the
 * frame-stability phase, and the viewport phase makes the NEXT occurrence say
 * "centre outside the viewport after scrolling" instead of blaming whatever the
 * button was supposed to do. The old whole-sequence retry also re-issued the
 * click itself; the shared contract retries only the preflight.
 */
export async function clickSnapped(target: Locator, label = 'element') {
  await clickWhenActionable(target, { label });
}


/**
 * Grant clipboard write where the permission EXISTS, and nowhere else.
 *
 * WebKit has no 'clipboard-write' permission, and asking for it is a hard error
 * — `browserContext.grantPermissions: Unknown permission: clipboard-write` —
 * which fails the test before the page loads. It allows the write under a user
 * gesture instead, which a real click is.
 *
 * A helper rather than an inline `if`, because the inline version is exactly
 * what did not get carried forward: it was written once in feedback-smoke.spec
 * (AMUX-3057), then omitted from msg-id-copy.spec, which reached CI as a
 * deterministic ios-safari failure. One import is harder to forget than one
 * remembered condition, and if the engine gap ever changes there is a single
 * place to change it.
 */
export async function grantClipboard(context: BrowserContext, browserName: string) {
  if (browserName === 'webkit') return;
  await context.grantPermissions(['clipboard-write']);
}
