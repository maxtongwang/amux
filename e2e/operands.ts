// recordOperands: on failure, attach the requests, DOM and page state behind a mismatched assertion — diagnostics only.
// Hosted e2e failure investigation, unresolved F01 / F03,F12 / F11: a status, title or count has no operand of its own.
import type { Page, Request, TestInfo } from '@playwright/test';
import { test as base, expect } from './fixtures';
import {
  buildOperandsReport,
  matchesPath,
  normalizeOperandOptions,
  operandsLine,
  redact,
  redactUrl,
  summarizeBody,
} from './operands-core.mjs';

export { expect };
export type { Page };

export interface OperandOptions {
  /** Pathname prefixes ('/api/board') or RegExps over path+query whose requests are kept. Required, non-empty. */
  paths: Array<string | RegExp>;
  /** Of those, the paths whose request and response bodies are kept as redacted JSON (the rest keep size + hash). */
  jsonPaths?: Array<string | RegExp>;
  /** Page globals (top-level let/const included; dotted paths allowed) read at each capture. */
  globals?: string[];
  /** CSS selectors whose elements' text, value, class, data-* and hidden state are read at each capture. */
  dom?: string[];
  /** Extra read-only page probe for state no global or selector exposes. Its result is redacted like the rest. */
  probe?: (page: Page) => Promise<unknown>;
  /** Names the attachment and the console line. */
  label?: string;
}

type Entry = Record<string, unknown>;
type Snapshot = { at: string; t_ms: number; globals?: unknown; dom?: unknown; probe?: unknown };

export interface OperandRecorder {
  /** Read the state now (never throws). Call it where a later step would change the state, e.g. first in a `finally`. */
  capture(at: string): Promise<void>;
  /** A timestamped step marker on the same clock as the requests. */
  mark(name: string): void;
  /** Detach, read a final snapshot, print one e2e_operands line; attach the JSON only when the test did not pass as expected. Never throws. */
  flush(testInfo: TestInfo): Promise<void>;
}

const BODY_WAIT_MS = 2_000;
// A wedged page must not turn the diagnostic into a fixture-teardown timeout.
const CAPTURE_BUDGET_MS = 5_000;

function within<T>(work: Promise<T>, ms: number, what: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const limit = new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error(`${what} exceeded ${ms}ms`)), ms); });
  return Promise.race([work, limit]).then(v => { clearTimeout(timer); return v; }, e => { clearTimeout(timer); throw e; });
}

// Runs in the page. Indirect eval reaches top-level let/const of classic
// scripts, which is where the dashboard keeps its state.
function readGlobalsInPage(names: string[]) {
  const seen = new WeakSet<object>();
  const safe = (v: unknown, depth: number): unknown => {
    if (v === null || v === undefined) return v ?? null;
    const t = typeof v;
    if (t === 'string' || t === 'number' || t === 'boolean') return v;
    if (t === 'function') return '[function]';
    if (t !== 'object') return String(v);
    if (typeof Element !== 'undefined' && v instanceof Element) return `[element ${v.tagName.toLowerCase()}${v.id ? '#' + v.id : ''}]`;
    if (seen.has(v as object)) return '[circular]';
    seen.add(v as object);
    if (depth > 5) return '[depth-capped]';
    if (v instanceof Map) return { '[map]': [...v.entries()].slice(0, 60).map(([k, x]) => [String(k), safe(x, depth + 1)]) };
    if (v instanceof Set) return { '[set]': [...v.values()].slice(0, 60).map(x => safe(x, depth + 1)) };
    if (Array.isArray(v)) return v.slice(0, 60).map(x => safe(x, depth + 1)).concat(v.length > 60 ? [`[+${v.length - 60} more]`] : []);
    const out: Record<string, unknown> = {};
    for (const k of Object.keys(v as object).slice(0, 60)) out[k] = safe((v as Record<string, unknown>)[k], depth + 1);
    return out;
  };
  const out: Record<string, unknown> = {};
  for (const name of names) {
    try {
      // eslint-disable-next-line no-eval
      out[name] = { defined: true, value: safe((0, eval)(name), 0) };
    } catch (e) {
      out[name] = { defined: false, error: String((e as Error)?.message || e).slice(0, 160) };
    }
  }
  return out;
}

// Runs in the page.
function readDomInPage(selectors: string[]) {
  const out: Record<string, unknown> = {};
  for (const sel of selectors) {
    try {
      const els = [...document.querySelectorAll(sel)];
      out[sel] = {
        count: els.length,
        items: els.slice(0, 25).map(el => {
          const h = el as HTMLElement & { value?: unknown; checked?: unknown };
          const item: Record<string, unknown> = {
            tag: el.tagName.toLowerCase(), id: el.id || undefined, class: el.className || undefined,
            text: (el.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 240),
            hidden: h.hidden || undefined, data: { ...h.dataset },
          };
          if ('value' in h && typeof h.value === 'string') item.value = h.value.slice(0, 240);
          if ('checked' in h && typeof h.checked === 'boolean') item.checked = h.checked;
          return item;
        }),
      };
    } catch (e) {
      out[sel] = { error: String((e as Error)?.message || e).slice(0, 160) };
    }
  }
  return out;
}

export function recordOperands(page: Page, options: OperandOptions): OperandRecorder {
  const opts = normalizeOperandOptions(options);
  const label = opts.label ?? 'operands';
  const t0 = Date.now();
  const entries: Entry[] = [];
  const marks: Array<{ name: string; t_ms: number }> = [];
  const snapshots: Snapshot[] = [];
  const snapshotErrors: string[] = [];
  const pending = new Set<Promise<void>>();
  let requestsSeen = 0;
  let listening = false;
  let flushed = false;

  const record = (req: Request, failure: string | null) => {
    requestsSeen++;
    const url = req.url();
    if (!matchesPath(url, opts.paths)) return;
    const json = opts.jsonPaths.length > 0 && matchesPath(url, opts.jsonPaths);
    const timing = req.timing();
    const entry: Entry = {
      method: req.method(), url: redactUrl(url), resource: req.resourceType(),
      t_start_ms: timing.startTime > 0 ? Math.round(timing.startTime - t0) : null,
      t_end_ms: Date.now() - t0,
      duration_ms: timing.responseEnd >= 0 ? Math.round(timing.responseEnd) : null,
      failure, status: null,
      // Request headers are never read: they carry the bearer and cookies.
      request_body: summarizeBody(req.postDataBuffer(), { json }),
    };
    entries.push(entry);
    if (failure) return;
    const done = (async () => {
      try {
        const resp = await req.response();
        if (!resp) { entry.status = 'no-response'; return; }
        entry.status = resp.status();
        try { entry.response_body = summarizeBody(await resp.body(), { json }); }
        catch (e) { entry.response_body = { unreadable: String((e as Error)?.message || e).split('\n')[0].slice(0, 160) }; }
      } catch (e) {
        entry.status = 'unreadable: ' + String((e as Error)?.message || e).split('\n')[0].slice(0, 160);
      }
    })();
    pending.add(done);
    void done.finally(() => pending.delete(done));
  };
  const onFinished = (req: Request) => record(req, null);
  const onFailed = (req: Request) => record(req, req.failure()?.errorText ?? 'failed');
  page.on('requestfinished', onFinished);
  page.on('requestfailed', onFailed);
  listening = true;

  const capture = async (at: string) => {
    const snap: Snapshot = { at, t_ms: Date.now() - t0 };
    try {
      if (page.isClosed()) throw new Error('page closed');
      await within((async () => {
        if (opts.globals.length) snap.globals = redact(await page.evaluate(readGlobalsInPage, opts.globals));
        if (opts.dom.length) snap.dom = redact(await page.evaluate(readDomInPage, opts.dom));
        if (opts.probe) snap.probe = redact(await opts.probe(page));
      })(), CAPTURE_BUDGET_MS, 'capture');
      snapshots.push(snap);
    } catch (e) {
      snapshotErrors.push(`${at}: ${String((e as Error)?.message || e).split('\n')[0].slice(0, 160)}`);
    }
  };

  return {
    capture,
    mark(name: string) { marks.push({ name: String(name).slice(0, 120), t_ms: Date.now() - t0 }); },
    async flush(testInfo: TestInfo) {
      if (flushed) return;
      flushed = true;
      try {
        await capture('teardown');
        await Promise.race([Promise.allSettled([...pending]), new Promise(r => setTimeout(r, BODY_WAIT_MS))]);
        page.off('requestfinished', onFinished);
        page.off('requestfailed', onFailed);
        const report = buildOperandsReport({
          label, test: testInfo.titlePath.slice(1).join(' > '), status: testInfo.status, expectedStatus: testInfo.expectedStatus,
          listening, requestsSeen, entries, marks, snapshots, snapshotErrors,
        });
        const failed = testInfo.status !== testInfo.expectedStatus;
        if (failed) {
          await testInfo.attach(`e2e-operands-${label.replace(/[^\w.-]+/g, '-')}.json`, {
            body: JSON.stringify(report, null, 2), contentType: 'application/json',
          });
        }
        console.log(operandsLine(report, failed));
      } catch (e) {
        // A diagnostic must never become the test's error.
        console.log('e2e_operands ' + JSON.stringify({ verdict: 'e2e_operands', label, measured: false,
          n_considered: entries.length, why_unmeasured: 'flush failed: ' + String((e as Error)?.message || e).split('\n')[0].slice(0, 160) }));
      }
    },
  };
}

/**
 * The shared fixture test plus `operands`: call `operands({...})` at the top
 * of a test to start a recorder; every recorder it started is flushed in
 * fixture teardown, after the test's status is known and before the page closes.
 */
export const test = base.extend<{ operands: (options: OperandOptions) => OperandRecorder }>({
  operands: async ({ page }, use, testInfo) => {
    const started: OperandRecorder[] = [];
    await use(options => {
      const recorder = recordOperands(page, options);
      started.push(recorder);
      return recorder;
    });
    for (const recorder of started) await recorder.flush(testInfo);
  },
});

/**
 * F11 probe: the inputs of the peek message filter for every loaded prompt.
 * `_peekPromptMatchesFilters` is the predicate `#peek-msg-count` counts, and
 * `_peekHasBoardRef` its board half; both only read. `cardId` is the board
 * card the spec injected, so whether it survived is answered directly.
 */
export function peekFilterProbe(cardId: string) {
  return (page: Page) => page.evaluate((id: string) => {
    const read = (0, eval);
    const items = read('boardItems');
    const list: Array<{ id?: unknown }> = Array.isArray(items) ? items : [];
    const hasRef = read('_peekHasBoardRef') as (t: string) => boolean;
    const matches = read('_peekPromptMatchesFilters') as (el: Element) => boolean;
    return {
      board_items: list.length,
      board_has_card: list.some(i => i && i.id === id),
      board_prefixes: [...new Set(list.map(i => String(i?.id || '').replace(/-\d+$/, '')).filter(Boolean))].slice(0, 40),
      prompts: [...document.querySelectorAll('#peek-body .peek-prompt')].slice(0, 25).map(el => ({
        kind: (el as HTMLElement).dataset.msgKind,
        text: (el.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 120),
        board_ref: hasRef(el.textContent || ''),
        matches_filters: matches(el),
      })),
    };
  }, cardId);
}
