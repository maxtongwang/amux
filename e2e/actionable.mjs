// Browser-free half of the e2e actionability contract (helpers.ts waitForActionable) — pure so node:test can pin it.
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
