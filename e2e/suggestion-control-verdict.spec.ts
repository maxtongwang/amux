// suggestion-control-verdict.spec coverage — use shared browser defaults and route diagnostics.
import { test, expect } from './fixtures';

// ATE-75 — a no-op suggestion probe must not turn into a silent Enter.
//
// This drives the shipped dashboard functions. The network is replaced at the
// fetch seam so both control responses are deterministic: the suggestion probe
// reports its measured no-effect verdict, then tmux accepts the fallback key
// without claiming what the foreground TUI did with it.
test('a missing suggestion names the unverified Enter fallback', async ({ page }) => {
  await page.goto('/');
  await page.waitForLoadState('networkidle');
  await page.waitForFunction(() => typeof (window as any)._submitSuggestion === 'function');

  const result = await page.evaluate(async () => {
    const w = window as any;
    const originalFetch = w.fetch;
    const originalToast = w.showToast;
    const calls: Array<{ path: string; body: any }> = [];
    const toasts: string[] = [];

    w.showToast = (message: unknown) => toasts.push(String(message));
    w.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), location.href);
      if (url.pathname.endsWith('/api/sessions/ate-75-probe/send')) {
        calls.push({ path: url.pathname, body: JSON.parse(String(init?.body || '{}')) });
        return new Response(JSON.stringify({
          ok: true,
          message: 'no suggestion found',
          submitted: false,
          submission: 'no_effect'
        }), { status: 200, headers: { 'Content-Type': 'application/json' } });
      }
      if (url.pathname.endsWith('/api/sessions/ate-75-probe/keys')) {
        calls.push({ path: url.pathname, body: JSON.parse(String(init?.body || '{}')) });
        return new Response(JSON.stringify({
          ok: true,
          accepted: true,
          effect: 'unverified',
          message: 'sent'
        }), { status: 200, headers: { 'Content-Type': 'application/json' } });
      }
      if (url.pathname.endsWith('/api/client-debug')) {
        return new Response('{}', { status: 200, headers: { 'Content-Type': 'application/json' } });
      }
      return originalFetch(input, init);
    };

    try {
      await w._submitSuggestion('ate-75-probe', false, 'Enter');
      return { calls, toasts };
    } finally {
      w.fetch = originalFetch;
      w.showToast = originalToast;
    }
  });

  expect(result.calls).toEqual([
    { path: '/api/sessions/ate-75-probe/send', body: { text: '' } },
    { path: '/api/sessions/ate-75-probe/keys', body: { keys: 'Enter' } }
  ]);
  expect(result.toasts).toContain(
    'No suggestion found — pressed Enter; effect unverified. If nothing changes, restart the worker.'
  );
  expect(result.toasts).not.toContain('Sent suggestion');
});

test('a rejected suggestion fallback names the recovery failure', async ({ page }) => {
  await page.goto('/');
  await page.waitForLoadState('networkidle');
  await page.waitForFunction(() => typeof (window as any)._submitSuggestion === 'function');

  const result = await page.evaluate(async () => {
    const w = window as any;
    const originalFetch = w.fetch;
    const calls: Array<{ path: string; body: any }> = [];
    const toasts: string[] = [];

    w.showToast = (message: unknown) => toasts.push(String(message));
    w.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), location.href);
      if (url.pathname.endsWith('/api/sessions/ate-75-rejected/send')) {
        calls.push({ path: url.pathname, body: JSON.parse(String(init?.body || '{}')) });
        return new Response(JSON.stringify({
          ok: true,
          message: 'no suggestion found',
          submitted: false,
          submission: 'no_effect'
        }), { status: 200, headers: { 'Content-Type': 'application/json' } });
      }
      if (url.pathname.endsWith('/api/sessions/ate-75-rejected/keys')) {
        calls.push({ path: url.pathname, body: JSON.parse(String(init?.body || '{}')) });
        return new Response(JSON.stringify({
          ok: false,
          accepted: false,
          effect: 'not_sent',
          message: 'not running'
        }), { status: 409, headers: { 'Content-Type': 'application/json' } });
      }
      if (url.pathname.endsWith('/api/client-debug')) {
        return new Response('{}', { status: 200, headers: { 'Content-Type': 'application/json' } });
      }
      return originalFetch(input, init);
    };

    try {
      await w._submitSuggestion('ate-75-rejected', false, 'Enter');
      return { calls, toasts };
    } finally {
      w.fetch = originalFetch;
    }
  });

  expect(result.calls).toEqual([
    { path: '/api/sessions/ate-75-rejected/send', body: { text: '' } },
    { path: '/api/sessions/ate-75-rejected/keys', body: { keys: 'Enter' } }
  ]);
  expect(result.toasts).toContain(
    'No suggestion found — Enter was not confirmed: key request was not confirmed'
  );
  expect(result.toasts.some((toast: string) => toast.includes('Suggestion control failed'))).toBeFalsy();
});
