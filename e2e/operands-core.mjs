// Browser-free half of the e2e operand recorder (operands.ts recordOperands) — pure so node:test can pin it.
// Hosted e2e failure investigation, unresolved state mismatches F01, F03/F12, F11: keep the operands, not only the verdict.
import { createHash } from 'node:crypto';

export const OPERANDS_VERDICT = 'e2e_operands';
export const MAX_ENTRIES = 300;
export const MAX_STRING = 240;
export const MAX_DEPTH = 6;
export const MAX_ITEMS = 60;
export const MAX_PARSE_BYTES = 256 * 1024;

// Credentials never reach an attachment, whatever key or query parameter they ride in.
// `token$` (not `token`) so usage counters such as input_tokens stay readable.
const SENSITIVE_KEY = /(authori[sz]ation|cookie|token$|secret|passw(or)?d|api[-_]?key|bearer|credential|private[-_]?key|session[-_]?id|signature|x-amz-)/i;
const SENSITIVE_VALUE = /^\s*(bearer|basic)\s+\S/i;
export const REDACTED = '[redacted]';
// A page global read by indirect eval: identifier syntax only, so a reader can never be a script.
const GLOBAL_NAME = /^[A-Za-z_$][\w$]*(\.[A-Za-z_$][\w$]*)*$/;

export function isSensitiveKey(key) {
  return SENSITIVE_KEY.test(String(key));
}

export function sha256(data) {
  return createHash('sha256').update(data).digest('hex');
}

/**
 * Deep copy for an attachment: credential-named keys and bearer-shaped values
 * are replaced, long strings keep a head plus the full value's length and hash,
 * and depth / item counts are capped with a marker saying what was cut.
 */
export function redact(value, opts = {}, depth = 0) {
  const maxString = opts.maxString ?? MAX_STRING;
  const maxDepth = opts.maxDepth ?? MAX_DEPTH;
  const maxItems = opts.maxItems ?? MAX_ITEMS;
  if (value === null || value === undefined) return value ?? null;
  if (typeof value === 'string') {
    if (SENSITIVE_VALUE.test(value)) return REDACTED;
    if (value.length <= maxString) return value;
    return value.slice(0, maxString) + `…[+${value.length - maxString} chars sha256:${sha256(value).slice(0, 12)}]`;
  }
  if (typeof value === 'number' || typeof value === 'boolean') return value;
  if (typeof value === 'bigint') return String(value);
  if (typeof value !== 'object') return `[${typeof value}]`;
  if (depth >= maxDepth) return Array.isArray(value) ? `[array(${value.length}) depth-capped]` : '[object depth-capped]';
  if (Array.isArray(value)) {
    const out = value.slice(0, maxItems).map(v => redact(v, opts, depth + 1));
    if (value.length > maxItems) out.push(`[+${value.length - maxItems} more items]`);
    return out;
  }
  const out = {};
  const keys = Object.keys(value);
  for (const k of keys.slice(0, maxItems)) out[k] = isSensitiveKey(k) ? REDACTED : redact(value[k], opts, depth + 1);
  if (keys.length > maxItems) out['[truncated]'] = `+${keys.length - maxItems} more keys`;
  return out;
}

/** Path plus query, with credential-named query values replaced. The origin is dropped: it is the test server. */
export function redactUrl(url) {
  let u;
  try { u = new URL(url); } catch { return '[unparseable url]'; }
  const params = [...u.searchParams.entries()].map(([k, v]) =>
    `${encodeURIComponent(k)}=${isSensitiveKey(k) ? REDACTED : encodeURIComponent(v.length > MAX_STRING ? v.slice(0, MAX_STRING) + '…' : v)}`);
  return u.pathname + (params.length ? '?' + params.join('&') : '');
}

/**
 * Validate recorder options at the boundary. `paths` are pathname prefixes
 * (strings) or RegExps tested against path+query; an empty list would record
 * nothing and read as a quiet page, so it is refused. `globals` are page-global
 * names (dotted paths allowed) read by indirect eval, so they are restricted to
 * identifier syntax: a reader, never a script. `dom` are CSS selectors.
 */
export function normalizeOperandOptions(options) {
  const o = options ?? {};
  if (typeof o !== 'object') throw new TypeError('operand options must be an object');
  const allowed = new Set(['paths', 'jsonPaths', 'globals', 'dom', 'probe', 'label']);
  for (const k of Object.keys(o)) if (!allowed.has(k)) throw new TypeError(`operand option "${k}" is not supported; allowed: ${[...allowed].join(', ')}`);
  const pathList = (name, list, required) => {
    if (list === undefined && !required) return [];
    if (!Array.isArray(list) || (required && list.length === 0)) throw new TypeError(`${name} must be a non-empty array of path prefixes or RegExps`);
    for (const p of list) {
      if (!(p instanceof RegExp) && !(typeof p === 'string' && p.startsWith('/'))) {
        throw new TypeError(`${name} entry ${String(p)} must be a RegExp or a pathname prefix starting with "/"`);
      }
    }
    return list;
  };
  const stringList = (name, list, valid, what) => {
    if (list === undefined) return [];
    if (!Array.isArray(list)) throw new TypeError(`${name} must be an array`);
    for (const v of list) if (typeof v !== 'string' || !valid(v)) throw new TypeError(`${name} entry ${String(v)} is not ${what}`);
    return list;
  };
  const paths = pathList('paths', o.paths, true);
  const jsonPaths = pathList('jsonPaths', o.jsonPaths, false);
  const globals = stringList('globals', o.globals, v => GLOBAL_NAME.test(v), 'a global name such as _mdaiCur.doc');
  const dom = stringList('dom', o.dom, v => v.trim().length > 0, 'a CSS selector');
  if (o.probe !== undefined && typeof o.probe !== 'function') throw new TypeError('probe must be a function');
  if (o.label !== undefined && typeof o.label !== 'string') throw new TypeError('label must be a string');
  return { paths, jsonPaths, globals, dom, probe: o.probe ?? null, label: o.label ?? null };
}

/** Does `url` fall under one of the patterns? Prefixes match the pathname; RegExps match path+query. */
export function matchesPath(url, patterns) {
  let u;
  try { u = new URL(url); } catch { return false; }
  const full = u.pathname + u.search;
  return patterns.some(p => (p instanceof RegExp ? (p.lastIndex = 0, p.test(full)) : u.pathname.startsWith(p)));
}

/**
 * A body as evidence: always its size and hash, and, when `json` is asked for
 * and it parses within the size cap, its redacted, capped content. A body
 * that was not readable says why rather than reading as empty.
 */
export function summarizeBody(body, { json = false } = {}) {
  if (body === null || body === undefined) return { bytes: 0, sha256: null };
  const buf = typeof body === 'string' ? Buffer.from(body, 'utf8') : Buffer.from(body);
  const out = { bytes: buf.length, sha256: buf.length ? sha256(buf).slice(0, 16) : null };
  if (!json || !buf.length) return out;
  if (buf.length > MAX_PARSE_BYTES) { out.json_skipped = `over ${MAX_PARSE_BYTES} bytes`; return out; }
  try { out.json = redact(JSON.parse(buf.toString('utf8'))); }
  catch { out.json_skipped = 'not json'; }
  return out;
}

/** Keep the newest `max` entries; how many were dropped travels with them. */
export function capEntries(entries, max = MAX_ENTRIES) {
  if (entries.length <= max) return { entries, dropped: 0 };
  return { entries: entries.slice(entries.length - max), dropped: entries.length - max };
}

/**
 * The attachment. `measured` is true only when the listeners ran AND the
 * snapshot was read; a zero `n_considered` beside `n_requests_seen` > 0 says
 * the path filter excluded traffic rather than that the page made none.
 */
export function buildOperandsReport({ label, test, status, expectedStatus, listening, requestsSeen, entries, marks = [], snapshots, snapshotErrors }) {
  const { entries: kept, dropped } = capEntries(entries);
  const snapOk = snapshots.length > 0;
  const why = [];
  if (!listening) why.push('network listeners were not installed');
  if (!snapOk) why.push('no snapshot was read' + (snapshotErrors.length ? ': ' + snapshotErrors.join('; ') : ''));
  return {
    verdict: OPERANDS_VERDICT,
    label,
    test,
    status,
    expected_status: expectedStatus,
    measured: listening && snapOk,
    ...(why.length ? { why_unmeasured: why.join('; ') } : {}),
    n_considered: entries.length,
    n_requests_seen: requestsSeen,
    dropped_entries: dropped,
    marks,
    requests: kept,
    snapshots,
    snapshot_errors: snapshotErrors,
  };
}

/** The one console line per recorded test: the sweepable summary, never the payload. */
export function operandsLine(report, attached) {
  return OPERANDS_VERDICT + ' ' + JSON.stringify({
    verdict: OPERANDS_VERDICT, label: report.label, test: report.test, status: report.status,
    measured: report.measured, n_considered: report.n_considered, n_requests_seen: report.n_requests_seen,
    snapshots: report.snapshots.map(s => s.at), attached,
    ...(report.why_unmeasured ? { why_unmeasured: report.why_unmeasured } : {}),
  });
}
