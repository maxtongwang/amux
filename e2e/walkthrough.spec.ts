// First-run walkthrough — explicit opt-in preserves onboarding coverage under shared defaults.
import { test, expect } from './fixtures';

test.use({ storageState: { cookies: [], origins: [] } });

test('an empty first visit automatically opens the walkthrough', {
  annotation: { type: 'walkthrough' },
}, async ({ page }) => {
  await page.route('**/api/sessions', route => route.fulfill({ json: [] }));
  await page.goto('/');
  await expect(page.locator('#wt-overlay.open')).toBeVisible();
  expect(await page.evaluate(() => localStorage.getItem('amux_walkthrough_done'))).toBeNull();
  await page.locator('#wt-tooltip .wt-skip').click();
  await expect(page.locator('#wt-overlay.open')).toHaveCount(0);
  expect(await page.evaluate(() => localStorage.getItem('amux_walkthrough_done'))).toBe('1');
});
