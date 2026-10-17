// Project config contract — catch lost defaults without evaluating the server-owning config.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { buildTargets, buildProject } from './playwright-projects.mjs';

// Inject a distinctive device descriptor; the config supplies Playwright's real registry.
const TARGETS = buildTargets({ 'iPhone 15': {
  viewport: { width: 393, height: 659 }, defaultBrowserType: 'webkit',
  isMobile: true, hasTouch: true, deviceScaleFactor: 3, userAgent: 'contract-device',
} });

test('the complete browser population is covered', () => {
  assert.deepEqual(TARGETS.map(t => t.name), ['desktop', 'mobile', 'ios-safari']);
});

for (const target of TARGETS) {
  test(`${target.name} preserves device settings and common use keys`, () => {
    const project = buildProject(target);
    const origin = `https://127.0.0.1:${target.port}`;
    assert.equal(project.name, target.name);
    for (const [key, value] of Object.entries(target.use)) {
      assert.deepEqual(project.use[key], value, `${target.name}: lost device key ${key}`);
    }
    assert.equal(project.use.baseURL, origin);
    assert.deepEqual(project.use.storageState, {
      cookies: [],
      origins: [{
        origin,
        localStorage: [{ name: 'amux_walkthrough_done', value: '1' }],
      }],
    }, `${target.name}: walkthrough suppression must survive project overrides`);
    assert.equal(project.use.trace ?? 'off', target.name === 'ios-safari' ? 'retain-on-failure' : 'off');
  });
}
