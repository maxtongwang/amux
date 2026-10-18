// peek-tool-header-wrap.spec coverage — use shared browser defaults and route diagnostics.
import { test, expect } from './fixtures';

// Ethan, 2026-09-27, with a screenshot of a Bash call in peek: "fix the
// formatting of how u show peek shit". The tool name read "B / a / s / h", one
// letter per line, and the linked log path sat in its own narrow column in the
// middle of the command.
//
// Cause: `.ptc-head` (the collapsible tool-call header) was `display: flex`, so
// every color span and every linked path in the header line became a separate
// flex item, and peek's break-anywhere wrapping let each shrink to one
// character. A survey of every live worker's peek found 169 such squeezed
// spans across 25 of 37 workers.
//
// The fixture runs the REAL render pipeline (`_peekHtml`) over raw terminal
// output with a bold ANSI tool name and an absolute path, because those two
// are what produce the separate spans. Render and measure in one `evaluate`:
// `#peek-body` is repainted by a poll, so a second round trip can measure a
// different frame (see peek-path-links.spec.ts).

// The path is long on purpose: a flex row only starves the tool name when an
// unbreakable neighbor wants the width. The first draft used a 40-character
// path and passed against the flex header it exists to catch.
const LONG = '/private/tmp/claude-501/-Users-ethan-Dev-mixpeek-studio/6d5ab9f5-f0f3-443e-9301-a9c3e6077bcb/scratchpad/signups_c4c.out';
const RAW = '\x1b[32m⏺\x1b[39m \x1b[1mBash\x1b[22m(F=' + LONG + '; git fetch -q origin main; '
  + 'tail -2 ' + LONG + ' | cut -c1-170; '
  + 'launchctl print gui/501/com.amux.server-rs-builder | grep -E "state =|runs =" | head -2)\n'
  + '  ⎿  health=db4b45ed5f3b\n'
  + '     pill change not live yet\n';

async function openPeek(page: import('@playwright/test').Page) {
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any).openPeek === 'function', { timeout: 20000 });
  await page.evaluate(() => (window as any).openPeek('e2e-probe'));
  await page.waitForSelector('#peek-overlay', { state: 'visible', timeout: 15000 });
}

function measure(page: import('@playwright/test').Page, light: boolean) {
  return page.evaluate(({ raw, light }) => {
    document.body.classList.toggle('light', light);
    const body = document.getElementById('peek-body')!;
    body.innerHTML = (window as any)._peekHtml(raw);
    const head = body.querySelector('.ptc-head') as HTMLElement | null;
    if (!head) return { head: false } as const;
    const lh = parseFloat(getComputedStyle(head).lineHeight) || 16;
    const squeezed = [...head.querySelectorAll('span, a')]
      .map((el) => ({ el, r: el.getBoundingClientRect(), t: (el.textContent || '').trim() }))
      // A short single word ("Bash") broken across lines is the defect; a
      // long path may wrap legitimately, so only words up to 12 characters.
      .filter(({ r, t }) => t.length >= 3 && t.length <= 12 && !/\s/.test(t) && r.height > lh * 1.5)
      .map(({ t, r }) => `${t.slice(0, 30)} (${Math.round(r.width)}x${Math.round(r.height)})`);
    const link = head.querySelector('.file-link') as HTMLElement | null;
    const rgb = (c: string) => (c.match(/[\d.]+/g) || []).slice(0, 3).map(Number);
    const lum = (c: string) => {
      const [r, g, b] = rgb(c).map((v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; });
      return 0.2126 * r + 0.7152 * g + 0.0722 * b;
    };
    const fg = link ? lum(getComputedStyle(link).color) : 0;
    const bg = lum(getComputedStyle(body).backgroundColor);
    const contrast = (Math.max(fg, bg) + 0.05) / (Math.min(fg, bg) + 0.05);
    document.body.classList.remove('light');
    return { head: true, squeezed, link: !!link, contrast: Math.round(contrast * 10) / 10 } as const;
  }, { raw: RAW, light });
}

for (const light of [false, true]) {
  test(`a tool-call header wraps as one line of text (${light ? 'light' : 'dark'} theme)`, async ({ page }) => {
    await openPeek(page);
    const m = await measure(page, light);
    // Preconditions: without a header and a linked path this spec is green on
    // nothing, which is how a fixture that stops producing the shape would pass.
    expect(m.head, 'the ⏺ line must render as a collapsible .ptc-head').toBe(true);
    expect(m.link, 'the absolute path must be linkified inside the header').toBe(true);
    expect(m.squeezed, 'no header piece may be squeezed into a one-letter column').toEqual([]);
    // The terminal is dark in both themes; a link colored for the light page
    // (#0550ae) measured 2.1:1 on it. 4.5 is WCAG AA for body text.
    expect(m.contrast, 'path links must stay readable on the dark terminal').toBeGreaterThanOrEqual(4.5);
  });
}
