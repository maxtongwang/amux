// Playwright project settings — pure construction keeps config contracts free of server setup.

export function buildTargets(devices) {
  return [
    { name: 'desktop', port: 18823, use: { viewport: { width: 1280, height: 800 } } },
    // Mobile is a first-class target (amux is mobile-first): 375px must
    // render without overflow.
    { name: 'mobile', port: 18833, use: { viewport: { width: 375, height: 667 } } },
    // iOS SAFARI IS ITS OWN TARGET, not a viewport (AMUX tab-customizer, 2026-08-13).
    // A bottom-sheet fix passed desktop+mobile Chromium at 375px while Ethan still
    // could not see the menu on his phone — Chromium at a phone WIDTH is not Safari
    // on a phone. WebKit differs on exactly what this UI depends on: position:fixed
    // inside a transformed ancestor, env(safe-area-inset-*), and dvh/vh behaviour.
    //
    // It runs the WHOLE suite. It briefly did not: a `testMatch` narrowed it to
    // four layout specs because the cross-project races above were misread as
    // iOS-specific reds. That scoping was honest about what it covered and still
    // wrong in effect — a new mobile spec would silently never reach iOS, which
    // is the one target that motivated the project. Isolation removed the reason
    // for it, so the list is gone; do not reintroduce one to quiet a red.
    { name: 'ios-safari', port: 18843, use: { ...devices['iPhone 15'] } },
  ];
}

/** @returns {NonNullable<import('@playwright/test').PlaywrightTestConfig['projects']>[number] & { name: string }} */
export function buildProject(t) {
  return {
    name: t.name,
    use: {
      ...t.use,
      // Retain failure traces on iOS; other projects keep the default recording policy.
      trace: t.name === 'ios-safari' ? 'retain-on-failure' : 'off',
      baseURL: `https://127.0.0.1:${t.port}`,
      // WALKTHROUGH OFF BY DEFAULT, like the config service-worker policy (AMUX-4986/4987). `_wtMaybeStart` opens it 1500ms after
      // load when `sessions.length === 0`, which is the NORMAL state of every CI
      // project's isolated server, and `#wt-tooltip` then eats the click until
      // the budget is gone.
      // A walkthrough test explicitly opts in via annotation and empty storageState.
      storageState: {
        cookies: [],
        origins: [{
          origin: `https://127.0.0.1:${t.port}`,
          localStorage: [{ name: 'amux_walkthrough_done', value: '1' }],
        }],
      },
    },
    // Preserve the existing cold-page setup budget; retries remain owned by the config.
    ...(t.name === 'ios-safari' ? { timeout: 90_000 } : {}),
  };
}
