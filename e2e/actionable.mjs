// The e2e actionability contract behind helpers.ts waitForActionable/clickWhenActionable — browser-free so node:test can pin it.
// Hosted e2e failure investigation, "Proposed bounded fix" item 2: one deadline, measured stability, a named failure.

export const ACTIONABLE_DEFAULT_BUDGET_MS = 15_000;
export const ACTIONABLE_VERDICT = 'e2e_not_actionable';
const RECT_TOLERANCE_PX = 0.5;
const OPTION_KEYS = new Set(['budgetMs', 'scroll', 'label', 'stableFrames']);

/**
 * The named failure. `details` holds every diagnostic field; `phase` and
 * `cover` are lifted to the top so an assertion or a reader of the error
 * needs no knowledge of the details layout.
 */
export class ActionableTimeout extends Error {
  constructor(message, details) {
    super(message);
    this.name = 'ActionableTimeout';
    this.details = details;
    this.phase = details.phase;
    this.cover = details.cover;
  }
}

/** The one budget ran out while a step was still pending. Internal to the preflight loop. */
export class DeadlineExceeded extends Error {
  constructor(what) {
    super(`actionability budget exhausted during ${what}`);
    this.name = 'DeadlineExceeded';
  }
}

/** Validate caller options at the boundary. Any click-bypass option (force, trial, timeout, position...) is refused. */
export function normalizeActionableOptions(options) {
  const o = options ?? {};
  if (typeof o !== 'object') throw new TypeError('actionable options must be an object');
  for (const k of Object.keys(o)) {
    if (!OPTION_KEYS.has(k)) {
      throw new TypeError(`actionable option "${k}" is not supported; allowed: ${[...OPTION_KEYS].join(', ')}. ` +
        'There is no way to skip actionability: fix what blocks the target instead.');
    }
  }
  const budgetMs = o.budgetMs ?? ACTIONABLE_DEFAULT_BUDGET_MS;
  if (typeof budgetMs !== 'number' || !Number.isFinite(budgetMs) || budgetMs <= 0) {
    throw new TypeError(`budgetMs must be a positive finite number, got ${String(budgetMs)}`);
  }
  const scroll = o.scroll ?? true;
  if (typeof scroll !== 'boolean') throw new TypeError('scroll must be a boolean');
  const stableFrames = o.stableFrames ?? 3;
  if (!Number.isInteger(stableFrames) || stableFrames < 2) {
    throw new TypeError('stableFrames must be an integer >= 2; one frame cannot show rest');
  }
  const label = o.label ?? null;
  if (label !== null && typeof label !== 'string') throw new TypeError('label must be a string');
  return { budgetMs, scroll, label, stableFrames };
}

/**
 * One total deadline. `timeout(cap)` is the value to hand a Playwright step:
 * never above the remaining budget, and never below 1, because Playwright
 * reads timeout 0 as "wait forever".
 */
export function createDeadline(budgetMs = ACTIONABLE_DEFAULT_BUDGET_MS, now = Date.now) {
  const startedAt = now();
  const elapsed = () => now() - startedAt;
  const remaining = () => Math.max(0, budgetMs - elapsed());
  return {
    budgetMs,
    elapsed,
    remaining,
    expired: () => remaining() <= 0,
    timeout(cap = Infinity) {
      const c = typeof cap === 'number' && cap > 0 ? cap : Infinity;
      return Math.max(1, Math.min(remaining(), c));
    },
  };
}

/** A step timeout this close to the deadline is the deadline ending (timers fire a few ms apart). */
const BUDGET_END_SLACK_MS = 50;

/** Settle with `promise`, or reject with DeadlineExceeded when the budget ends first. */
export function raceDeadline(promise, deadline, what) {
  Promise.resolve(promise).catch(() => {}); // a loser rejecting later is not an unhandled rejection
  const ms = deadline.remaining();
  if (ms <= 0) return Promise.reject(new DeadlineExceeded(what));
  let timer;
  const expiry = new Promise((_, reject) => { timer = setTimeout(() => reject(new DeadlineExceeded(what)), ms); });
  return Promise.race([promise, expiry]).finally(() => clearTimeout(timer));
}

const isRect = r => r != null && ['x', 'y', 'width', 'height'].every(k => Number.isFinite(r[k]));

/** Two bounding boxes are the same resting geometry (sub-pixel jitter tolerated). Malformed input is never stable. */
export function rectsStable(a, b, tolerance = RECT_TOLERANCE_PX) {
  if (!isRect(a) || !isRect(b)) return false;
  return ['x', 'y', 'width', 'height'].every(k => Math.abs(a[k] - b[k]) <= tolerance);
}

/** Every consecutive pair of animation-frame samples is stable; fewer than two samples prove nothing. */
export function stableRun(samples) {
  if (!Array.isArray(samples) || samples.length < 2) return false;
  for (let i = 1; i < samples.length; i++) if (!rectsStable(samples[i - 1], samples[i])) return false;
  return true;
}

/** The click lands at the box centre, so that point must be inside the viewport. */
export function centreInViewport(rect, viewport) {
  if (!isRect(rect) || !viewport || !(viewport.width > 0) || !(viewport.height > 0)) return false;
  if (rect.width <= 0 || rect.height <= 0) return false;
  const cx = rect.x + rect.width / 2;
  const cy = rect.y + rect.height / 2;
  return cx >= 0 && cy >= 0 && cx < viewport.width && cy < viewport.height;
}

/** `tag#id.class > tag.class`, outermost first; null when there is no cover. */
export function formatElementPath(cover) {
  if (!cover || !Array.isArray(cover.chain) || cover.chain.length === 0) return null;
  const parts = cover.chain.map(n => `${n.tag}${n.id ? `#${n.id}` : ''}${(n.classes || []).map(c => `.${c}`).join('')}`);
  return (cover.truncated ? '… > ' : '') + parts.join(' > ');
}

/** Playwright's "Call log:" section of an error message, one entry per line, ANSI stripped. */
export function extractCallLog(message) {
  if (typeof message !== 'string') return null;
  const at = message.indexOf('Call log:');
  if (at < 0) return null;
  const lines = message.slice(at + 'Call log:'.length).replace(/\u001b\[[0-9;]*m/g, '')
    .split('\n').map(l => l.trim()).filter(Boolean);
  return lines.length ? lines : null;
}

const fmtRect = r => (isRect(r) ? `${Math.round(r.x)},${Math.round(r.y)} ${Math.round(r.width)}x${Math.round(r.height)}` : 'unknown');

/**
 * Build the named error and its one-line measured verdict. `cover` null with a
 * `coverWhy` means the probe could not run; null without one means it ran and
 * the centre hit the target itself.
 */
export function buildNotActionable(f) {
  const cover = formatElementPath(f.cover);
  const coverMeasured = !f.coverWhy;
  const details = {
    project: f.project ?? null,
    shard: f.shard ?? null,
    worker: f.worker ?? null,
    elapsed_ms: f.elapsedMs,
    budget_ms: f.budgetMs,
    selector: f.selector,
    label: f.label ?? null,
    phase: f.phase,
    reason: f.reason ?? null,
    // The phase the budget cut into, when that was not the reported one: the
    // verdict is the last FINISHED attempt's, not the interrupted step's.
    interrupted_phase: f.interruptedPhase ?? null,
    rect: f.rect ?? null,
    viewport: f.viewport ?? null,
    cover,
    cover_measured: coverMeasured,
    cover_why: f.coverWhy ?? null,
    screenshot: f.screenshot ?? null,
    call_log: f.callLog ?? null,
  };
  const coverText = cover ? `covered by ${cover}` : coverMeasured ? 'not covered' : `cover unmeasured (${f.coverWhy})`;
  const vp = details.viewport ? `${details.viewport.width}x${details.viewport.height}` : 'unknown';
  const message = [
    `ActionableTimeout: ${details.label ?? 'target'} (${details.selector}) not actionable at phase "${details.phase}" ` +
      `after ${details.elapsed_ms}/${details.budget_ms}ms: ${details.reason ?? 'no reason recorded'}; ${coverText}.`,
    `project=${details.project ?? 'unknown'} shard=${details.shard ?? 'unknown'} worker=${details.worker ?? 'unknown'} ` +
      `rect=${fmtRect(details.rect)} viewport=${vp} screenshot=${details.screenshot ?? 'none'}`,
    'This is a HARNESS diagnosis about reaching the control, not a verdict on the feature behind it; ' +
      'do not file it against the product (AF-47).',
    details.call_log ? `Playwright call log:\n  ${details.call_log.join('\n  ')}` : 'Playwright call log: none captured',
  ].join('\n');
  const line = JSON.stringify({ verdict: ACTIONABLE_VERDICT, measured: true, n_considered: 1, ...details });
  return { error: new ActionableTimeout(message, details), line };
}

// ── The preflight loop and the single click ────────────────────────────────
// They live here, over a Playwright-shaped locator, so node:test can drive
// them with fake locators (actionable.test.mjs). helpers.ts supplies `env`:
// the browser-only failure context (cover probe, screenshot, project/shard).

/** Browser-side: sample the target's box on `frames` consecutive animation frames. Serialized into the page. */
export function sampleFrames(el, frames) {
  return new Promise((resolve) => {
    const rects = [];
    const take = () => {
      const r = el.getBoundingClientRect();
      rects.push({ x: r.x, y: r.y, width: r.width, height: r.height });
      if (rects.length >= frames) resolve({ rects, viewport: { width: innerWidth, height: innerHeight } });
      else requestAnimationFrame(take);
    };
    requestAnimationFrame(take);
  });
}

const nextFrame = (target, deadline) =>
  raceDeadline(target.page().evaluate(() => new Promise((r) => requestAnimationFrame(() => r(null)))), deadline, 'frame pacing');

const firstLine = e => String(e?.message ?? e).split('\n')[0];

async function notActionable(target, label, deadline, phase, reason, lastError, env, interruptedPhase) {
  const elapsedMs = deadline.elapsed();
  const context = env?.failureContext ? await env.failureContext(target, label) : { coverWhy: 'no failure probe supplied' };
  const { error, line } = buildNotActionable({
    ...context,
    elapsedMs,
    budgetMs: deadline.budgetMs,
    selector: String(target),
    label,
    phase,
    reason,
    interruptedPhase,
    callLog: extractCallLog(lastError?.message),
  });
  (env?.log ?? console.log)(line);
  return error;
}

/**
 * The waitForActionable contract (documented on the helpers.ts export), over
 * any Playwright-shaped locator. Every step is given at most the remaining
 * budget, so the one deadline bounds the whole preflight.
 */
export async function waitForActionableWith(target, options, env) {
  const opts = normalizeActionableOptions(options);
  const deadline = createDeadline(opts.budgetMs);
  let phase = 'resolve';
  let reason = 'no attempt completed';
  let lastError = null;
  // The last FINISHED attempt's verdict. A budget that runs out early in a fresh
  // attempt must not overwrite it, or a covered target would report "resolve".
  let outcome = null;
  let interrupted = null;
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
      // Playwright's own timeout on a step that was handed the rest of the
      // budget is the deadline ending too, not a finding about the target.
      // Recorded as one, it replaced a finished "viewport" verdict with
      // "enabled step threw: Timeout 2ms exceeded" (e2e control 7ca1c0d3).
      const budgetEnded = e?.name === 'TimeoutError' && phase !== 'hit-test' && deadline.remaining() <= BUDGET_END_SLACK_MS;
      if (e instanceof DeadlineExceeded || budgetEnded) {
        if (!outcome) reason = `budget ended during ${phase}`;
        else if (outcome.phase !== phase) interrupted = phase;
        break;
      }
      lastError = e; // keep Playwright's own error: its message carries the call log
      if (phase !== 'hit-test') reason = `${phase} step threw: ${firstLine(e)}`;
      finish();
      // The trial click was given the whole remaining budget, so its timeout IS
      // the deadline; stop here and keep its call log ("... intercepts pointer events").
      if (phase === 'hit-test' && e?.name === 'TimeoutError') break;
      await nextFrame(target, deadline).catch(() => {}); // pace a fast-failing step; expiry ends the loop
    }
  }
  const final = outcome ?? { phase, reason };
  if (target.page().isClosed()) final.reason = `page closed during ${phase}; ${final.reason}`;
  throw await notActionable(target, opts.label, deadline, final.phase, final.reason, lastError, env, interrupted);
}

/**
 * waitForActionableWith, then exactly ONE real click with the remaining budget.
 * The click is never retried: it may already have been delivered, and a second
 * delivery of a business action is a different bug. Assert its effect after.
 */
export async function clickWhenActionableWith(target, options, env) {
  const label = normalizeActionableOptions(options).label;
  const { deadline } = await waitForActionableWith(target, options, env);
  try {
    await target.click({ timeout: deadline.timeout() });
  } catch (e) {
    throw await notActionable(target, label, deadline, 'click',
      'the single real click failed after a passing preflight (re-render or new cover in between)', e, env);
  }
}
