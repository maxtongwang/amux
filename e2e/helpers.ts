// Shared e2e helpers: the one actionability contract every harness click goes through, and engine-aware permissions.
// Hosted e2e failure investigation, "Proposed bounded fix" item 2 (overlay interception, F05/F08/F09/F13/F16).
import { test, BrowserContext, Locator } from '@playwright/test';
import {
  ActionableTimeout,
  DeadlineExceeded,
  buildNotActionable,
  centreInViewport,
  createDeadline,
  extractCallLog,
  normalizeActionableOptions,
  raceDeadline,
  stableRun,
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

// Browser-side probes. Self-contained: Playwright serializes them into the page.
function sampleFrames(el: Element, frames: number) {
  return new Promise<{ rects: Rect[]; viewport: Viewport }>((resolve) => {
    const rects: Rect[] = [];
    const take = () => {
      const r = el.getBoundingClientRect();
      rects.push({ x: r.x, y: r.y, width: r.width, height: r.height });
      if (rects.length >= frames) resolve({ rects, viewport: { width: innerWidth, height: innerHeight } });
      else requestAnimationFrame(take);
    };
    requestAnimationFrame(take);
  });
}

function probeCover(el: Element) {
  const r = el.getBoundingClientRect();
  const rect = { x: r.x, y: r.y, width: r.width, height: r.height };
  const viewport = { width: innerWidth, height: innerHeight };
  const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
  if (!hit) return { rect, viewport, cover: null, why: 'the target centre is outside the viewport; nothing to hit-test' };
  if (hit === el || el.contains(hit)) return { rect, viewport, cover: null, why: null };
  const chain: CoverNode[] = [];
  let n: Element | null = hit;
  while (n && n !== document.documentElement && chain.length < 8) {
    chain.unshift({ tag: n.tagName.toLowerCase(), id: n.id || '', classes: Array.from(n.classList) });
    n = n.parentElement;
  }
  return { rect, viewport, cover: { chain, truncated: !!n && n !== document.documentElement }, why: null };
}

const nextFrame = (target: Locator, deadline: Deadline) =>
  raceDeadline(target.page().evaluate(() => new Promise((r) => requestAnimationFrame(() => r(null)))), deadline, 'frame pacing');

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
 */
export async function waitForActionable(target: Locator, options?: ActionableOptions): Promise<Actionable> {
  const opts = normalizeActionableOptions(options);
  const deadline = createDeadline(opts.budgetMs);
  let phase = 'resolve';
  let reason = 'no attempt completed';
  let lastError: unknown = null;
  // The last FINISHED attempt's verdict. A budget that runs out early in a fresh
  // attempt must not overwrite it, or a covered target would report "resolve".
  let outcome: { phase: string; reason: string } | null = null;
  const finish = () => { outcome = { phase, reason }; };
  while (!deadline.expired() && !target.page().isClosed()) {
    try {
      phase = 'resolve';
      const n = await raceDeadline(target.count(), deadline, 'resolve');
      if (n !== 1) {
        reason = `${n} elements match; exactly one attached element is required`;
        finish();
        await nextFrame(target, deadline);
        continue;
      }
      phase = 'visible';
      if (!(await raceDeadline(target.isVisible(), deadline, 'visible'))) {
        reason = 'attached but not visible';
        finish();
        await nextFrame(target, deadline);
        continue;
      }
      phase = 'enabled';
      if (!(await target.isEnabled({ timeout: deadline.timeout(2_000) }))) {
        reason = 'visible but disabled';
        finish();
        await nextFrame(target, deadline);
        continue;
      }
      if (opts.scroll) {
        phase = 'scroll';
        await target.scrollIntoViewIfNeeded({ timeout: deadline.timeout(3_000) });
      }
      phase = 'stable';
      const sample = await raceDeadline(
        target.evaluate(sampleFrames, opts.stableFrames, { timeout: deadline.timeout() }), deadline, 'stability sampling');
      const rect = sample.rects[sample.rects.length - 1];
      if (!stableRun(sample.rects)) {
        reason = `bounding box still moving across ${opts.stableFrames} animation frames`;
        finish();
        continue; // the sampling itself waited those frames
      }
      phase = 'viewport';
      if (!centreInViewport(rect, sample.viewport)) {
        reason = opts.scroll
          ? 'at rest with its centre outside the viewport after scrolling'
          : 'centre outside the viewport and scrolling is disabled for this check';
        finish();
        await nextFrame(target, deadline);
        continue;
      }
      phase = 'hit-test';
      reason = 'trial click did not pass (another element receives the pointer events, or the target changed)';
      await target.click({ trial: true, timeout: deadline.timeout() });
      return { deadline, rect };
    } catch (e) {
      if (e instanceof DeadlineExceeded) {
        if (!outcome) reason = `budget ended during ${phase}`;
        break;
      }
      lastError = e; // keep Playwright's own error: its message carries the call log
      if (phase !== 'hit-test') reason = `${phase} step threw: ${firstLine(e)}`;
      finish();
      // The trial click was given the whole remaining budget, so its timeout IS
      // the deadline; stop here and keep its call log ("... intercepts pointer events").
      if (phase === 'hit-test' && (e as Error)?.name === 'TimeoutError') break;
      await nextFrame(target, deadline).catch(() => {}); // pace a fast-failing step; expiry ends the loop
    }
  }
  const final: { phase: string; reason: string } = outcome ?? { phase, reason };
  if (target.page().isClosed()) final.reason = `page closed during ${phase}; ${final.reason}`;
  throw await notActionable(target, opts.label, deadline, final.phase, final.reason, lastError);
}

/**
 * waitForActionable, then exactly ONE real click with the remaining budget.
 * The click is never retried: it may already have been delivered, and a second
 * delivery of a business action is a different bug. Assert its effect after.
 */
export async function clickWhenActionable(target: Locator, options?: ActionableOptions): Promise<void> {
  const label = normalizeActionableOptions(options).label;
  const { deadline } = await waitForActionable(target, options);
  try {
    await target.click({ timeout: deadline.timeout() });
  } catch (e) {
    throw await notActionable(target, label, deadline, 'click',
      'the single real click failed after a passing preflight (re-render or new cover in between)', e);
  }
}

function firstLine(e: unknown): string {
  return String((e as Error)?.message ?? e).split('\n')[0];
}

async function notActionable(
  target: Locator, label: string | null, deadline: Deadline, phase: string, reason: string, lastError: unknown,
): Promise<ActionableTimeout> {
  const elapsedMs = deadline.elapsed();
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
  const { error, line } = buildNotActionable({
    project: info?.project.name ?? null,
    shard: process.env.AMUX_E2E_SHARD || (shardCfg ? `${shardCfg.current}/${shardCfg.total}` : null),
    worker: info ? info.workerIndex : null,
    elapsedMs,
    budgetMs: deadline.budgetMs,
    selector: String(target),
    label,
    phase,
    reason,
    rect,
    viewport,
    cover,
    coverWhy,
    screenshot,
    callLog: extractCallLog((lastError as Error)?.message),
  });
  console.log(line);
  return error;
}

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
