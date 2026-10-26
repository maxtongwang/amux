// Multi-network multiplayer: two users on separate "machines" (browser
// contexts) join via group-scoped invites, create isolated fleet slices,
// share a board with concurrent mutations, and survive selective revocation.
import { test, expect } from './fixtures';
import type { Page } from '@playwright/test';

async function settle(page: Page): Promise<void> {
  await expect(page.locator('#conn-status').first()).toBeAttached();
  await page.waitForFunction(() => typeof (window as any).apiCall === 'function');
  await page.addLocatorHandler(page.locator('#sw-fail-bar'), async (bar) => {
    await bar.locator('button').last().click();
  });
  const walkthrough = page.locator('#wt-overlay.open');
  await walkthrough.waitFor({ state: 'visible', timeout: 8_000 }).catch(() => {});
  if (await walkthrough.isVisible()) {
    await page.locator('#wt-tooltip .wt-skip').click();
    await expect(walkthrough).toBeHidden();
  }
}

test('multi-network: isolated guests share workspace, scoped by group, with concurrent mutation and selective revocation', async ({
  page: owner,
  browser,
  request,
}) => {
  test.setTimeout(120_000);
  await owner.goto('/');
  await settle(owner);
  const ownerToken = await owner.evaluate(() => (window as any)._AMUX_AUTH_TOKEN as string);
  const ownerUiToken = await owner.evaluate(() => (window as any)._AMUX_UI_TOKEN as string);
  expect(ownerToken).toBeTruthy();
  expect(ownerUiToken).toBeTruthy();
  const ownerHeaders = {
    Authorization: `Bearer ${ownerToken}`,
    'Content-Type': 'application/json',
  };

  const ts = Date.now();
  const createdWorkers: string[] = [];
  const createdCards: string[] = [];

  // Seed workers establish the "alpha" and "beta" groups so scoped invites
  // can resolve their target (resolve_scope_target checks CC_TAGS).
  const seedAlpha = `e2e-seed-alpha-${ts}`;
  const seedBeta = `e2e-seed-beta-${ts}`;
  for (const [name, tag] of [[seedAlpha, 'alpha'], [seedBeta, 'beta']] as const) {
    const resp = await request.post('/api/sessions', {
      headers: ownerHeaders,
      data: { name, dir: '/tmp', provider: 'codex', tags: [tag], start: false },
    });
    expect(resp.status()).toBe(201);
    expect((await resp.json()).starting, `${name} must not autostart`).toBe(false);
    createdWorkers.push(name);
  }

  // 1. Owner creates two group-scoped invites via the API.
  const alphaInviteResp = await request.post('/api/org/invites', {
    headers: ownerHeaders,
    data: { email: 'alpha@example.com', scope_level: 'group', scope_name: 'alpha' },
  });
  expect(alphaInviteResp.status()).toBe(201);
  const alphaInvite = await alphaInviteResp.json();
  expect(alphaInvite).toMatchObject({ scope_level: 'group', scope_name: 'alpha' });

  const betaInviteResp = await request.post('/api/org/invites', {
    headers: ownerHeaders,
    data: { email: 'beta@example.com', scope_level: 'group', scope_name: 'beta' },
  });
  expect(betaInviteResp.status()).toBe(201);
  const betaInvite = await betaInviteResp.json();
  expect(betaInvite).toMatchObject({ scope_level: 'group', scope_name: 'beta' });

  // 2. Two separate browser contexts simulate different machines/networks.
  const alphaCtx = await browser.newContext({ ignoreHTTPSErrors: true, serviceWorkers: 'block' });
  const betaCtx = await browser.newContext({ ignoreHTTPSErrors: true, serviceWorkers: 'block' });
  const alphaPage = await alphaCtx.newPage();
  const betaPage = await betaCtx.newPage();

  try {
    // Accept alpha invite
    await alphaPage.goto(`/invite/${alphaInvite.token}`);
    await expect(alphaPage.getByRole('heading', { name: /^Join / })).toBeVisible();
    await expect(alphaPage.locator('#email')).toHaveValue('alpha@example.com');
    await alphaPage.locator('#name').fill('Alpha User');
    await Promise.all([
      alphaPage.waitForURL((url) => url.pathname === '/'),
      alphaPage.getByRole('button', { name: 'Join workspace' }).click(),
    ]);
    await settle(alphaPage);

    // Accept beta invite
    await betaPage.goto(`/invite/${betaInvite.token}`);
    await expect(betaPage.getByRole('heading', { name: /^Join / })).toBeVisible();
    await expect(betaPage.locator('#email')).toHaveValue('beta@example.com');
    await betaPage.locator('#name').fill('Beta User');
    await Promise.all([
      betaPage.waitForURL((url) => url.pathname === '/'),
      betaPage.getByRole('button', { name: 'Join workspace' }).click(),
    ]);
    await settle(betaPage);

    // Verify /api/identity: correct email, is_local_member, access_scope
    const alphaIdentity = await alphaPage.evaluate(async () => {
      const r = await fetch('/api/identity');
      return { status: r.status, body: await r.json() };
    });
    expect(alphaIdentity.status).toBe(200);
    expect(alphaIdentity.body).toMatchObject({
      email: 'alpha@example.com',
      is_local_member: true,
      access_scope: { level: 'group', name: 'alpha' },
    });

    const betaIdentity = await betaPage.evaluate(async () => {
      const r = await fetch('/api/identity');
      return { status: r.status, body: await r.json() };
    });
    expect(betaIdentity.status).toBe(200);
    expect(betaIdentity.body).toMatchObject({
      email: 'beta@example.com',
      is_local_member: true,
      access_scope: { level: 'group', name: 'beta' },
    });

    // 3. Group-scoped members cannot create workers (fleet mutation is
    // owner-only). Rescope both to global so they CAN create, then rescope
    // back to groups for isolation testing.
    const members = await (
      await request.get('/api/org/members', { headers: ownerHeaders })
    ).json();
    const alphaMember = members.find((m: any) => m.email === 'alpha@example.com');
    const betaMember = members.find((m: any) => m.email === 'beta@example.com');
    expect(alphaMember).toBeTruthy();
    expect(betaMember).toBeTruthy();

    // Temporarily elevate to global so guests can create workers
    for (const member of [alphaMember, betaMember]) {
      const resp = await request.patch(
        `/api/org/members/${encodeURIComponent(member.id)}`,
        { headers: ownerHeaders, data: { scope_level: 'global', grant_host_access: true } }, // d071f794: global needs an explicit host-access grant
      );
      expect(resp.status()).toBe(200);
    }

    // Each guest creates workers in their group
    const alphaWorker = `e2e-alpha-worker-${ts}`;
    const betaWorker = `e2e-beta-worker-${ts}`;

    const alphaCreate = await alphaPage.evaluate(async (name) => {
      const r = await fetch('/api/sessions', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name, dir: '/tmp', provider: 'codex', tags: ['alpha'] }),
      });
      return { status: r.status, body: await r.json() };
    }, alphaWorker);
    expect(alphaCreate.status).toBe(201);
    expect(alphaCreate.body.creator).toBe('member:alpha@example.com');
    createdWorkers.push(alphaWorker);

    const betaCreate = await betaPage.evaluate(async (name) => {
      const r = await fetch('/api/sessions', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name, dir: '/tmp', provider: 'codex', tags: ['beta'] }),
      });
      return { status: r.status, body: await r.json() };
    }, betaWorker);
    expect(betaCreate.status).toBe(201);
    expect(betaCreate.body.creator).toBe('member:beta@example.com');
    createdWorkers.push(betaWorker);

    // 5. Spoof resistance: alpha sends spoofed X-Amux-Worker and creator
    // headers; server must override with verified member identity.
    const spoofName = `e2e-spoof-worker-${ts}`;
    const spoofResult = await alphaPage.evaluate(async (name) => {
      const r = await fetch('/api/sessions', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'X-Amux-Worker': 'spoofed-identity',
        },
        body: JSON.stringify({
          name,
          dir: '/tmp',
          provider: 'codex',
          creator: 'spoofed-owner',
          tags: ['alpha'],
        }),
      });
      return { status: r.status, body: await r.json() };
    }, spoofName);
    expect(spoofResult.status).toBe(201);
    expect(spoofResult.body.creator).toBe('member:alpha@example.com');
    createdWorkers.push(spoofName);

    // Also test spoof resistance on board card creation
    const spoofCardResult = await alphaPage.evaluate(async (session) => {
      const r = await fetch('/api/board', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'X-Amux-Worker': 'spoofed-card-author',
        },
        body: JSON.stringify({
          title: `spoof-test-${Date.now()}`,
          type: 'chore',
          status: 'todo',
          session,
          creator: 'spoofed-owner',
        }),
      });
      return { status: r.status, body: await r.json() };
    }, alphaWorker);
    expect(spoofCardResult.status).toBe(201);
    expect(spoofCardResult.body.creator).toBe('member:alpha@example.com');
    createdCards.push(spoofCardResult.body.id);

    // 4. Each guest creates board cards assigned to their workers.
    const alphaCardTitle = `alpha-card-${ts}`;
    const betaCardTitle = `beta-card-${ts}`;

    const alphaCard = await alphaPage.evaluate(async ({ title, session }) => {
      const r = await fetch('/api/board', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ title, type: 'chore', status: 'todo', session }),
      });
      return { status: r.status, body: await r.json() };
    }, { title: alphaCardTitle, session: alphaWorker });
    expect(alphaCard.status).toBe(201);
    expect(alphaCard.body.creator).toBe('member:alpha@example.com');
    createdCards.push(alphaCard.body.id);

    const betaCard = await betaPage.evaluate(async ({ title, session }) => {
      const r = await fetch('/api/board', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ title, type: 'chore', status: 'todo', session }),
      });
      return { status: r.status, body: await r.json() };
    }, { title: betaCardTitle, session: betaWorker });
    expect(betaCard.status).toBe(201);
    expect(betaCard.body.creator).toBe('member:beta@example.com');
    createdCards.push(betaCard.body.id);

    // Owner sees both cards with correct attribution
    const ownerAlpha = await (
      await request.get(`/api/board/${encodeURIComponent(alphaCard.body.id)}`, {
        headers: ownerHeaders,
      })
    ).json();
    expect(ownerAlpha.creator).toBe('member:alpha@example.com');

    const ownerBeta = await (
      await request.get(`/api/board/${encodeURIComponent(betaCard.body.id)}`, {
        headers: ownerHeaders,
      })
    ).json();
    expect(ownerBeta.creator).toBe('member:beta@example.com');

    // 6. Rescope to group before testing isolation
    const rescopeAlpha = await request.patch(
      `/api/org/members/${encodeURIComponent(alphaMember.id)}`,
      { headers: ownerHeaders, data: { scope_level: 'group', scope_name: 'alpha' } },
    );
    expect(rescopeAlpha.status()).toBe(200);

    const rescopeBeta = await request.patch(
      `/api/org/members/${encodeURIComponent(betaMember.id)}`,
      { headers: ownerHeaders, data: { scope_level: 'group', scope_name: 'beta' } },
    );
    expect(rescopeBeta.status()).toBe(200);

    // Verify rescoped identity
    const alphaRescoped = await alphaPage.evaluate(async () => {
      const r = await fetch('/api/identity');
      return (await r.json()).access_scope;
    });
    expect(alphaRescoped).toEqual({ level: 'group', name: 'alpha' });

    const betaRescoped = await betaPage.evaluate(async () => {
      const r = await fetch('/api/identity');
      return (await r.json()).access_scope;
    });
    expect(betaRescoped).toEqual({ level: 'group', name: 'beta' });

    // Scope isolation: alpha can't see beta's workers, beta can't see alpha's
    const alphaIsolation = await alphaPage.evaluate(async (betaW) => {
      const info = await fetch(`/api/sessions/${encodeURIComponent(betaW)}/info`);
      // The listing can answer a transient 503 while the single fleet builder
      // is busy; retry like getSessionsResilient, and say what came back.
      let res = await fetch('/api/sessions');
      for (let i = 1; i < 5 && !res.ok; i++) { await new Promise(r => setTimeout(r, 250 * i)); res = await fetch('/api/sessions'); }
      const fleet = await res.json();
      return {
        infoStatus: info.status,
        fleetStatus: res.status,
        fleetNames: Array.isArray(fleet) ? fleet.map((r: any) => r.name) : fleet,
      };
    }, betaWorker);
    expect(alphaIsolation.infoStatus).toBe(403);
    expect(alphaIsolation.fleetStatus, JSON.stringify(alphaIsolation.fleetNames)).toBe(200);
    expect(alphaIsolation.fleetNames).not.toContain(betaWorker);

    const betaIsolation = await betaPage.evaluate(async (alphaW) => {
      const info = await fetch(`/api/sessions/${encodeURIComponent(alphaW)}/info`);
      // The listing can answer a transient 503 while the single fleet builder
      // is busy; retry like getSessionsResilient, and say what came back.
      let res = await fetch('/api/sessions');
      for (let i = 1; i < 5 && !res.ok; i++) { await new Promise(r => setTimeout(r, 250 * i)); res = await fetch('/api/sessions'); }
      const fleet = await res.json();
      return {
        infoStatus: info.status,
        fleetStatus: res.status,
        fleetNames: Array.isArray(fleet) ? fleet.map((r: any) => r.name) : fleet,
      };
    }, alphaWorker);
    expect(betaIsolation.infoStatus).toBe(403);
    expect(betaIsolation.fleetStatus, JSON.stringify(betaIsolation.fleetNames)).toBe(200);
    expect(betaIsolation.fleetNames).not.toContain(alphaWorker);

    // 7. Concurrent desc_append: a shared worker visible to both groups lets
    // both guests write to the same card. The authorize middleware checks
    // the card's session against the member scope, so the worker must carry
    // both tags.
    const sharedWorker = `e2e-shared-worker-${ts}`;
    const sharedResp = await request.post('/api/sessions', {
      headers: ownerHeaders,
      data: { name: sharedWorker, dir: '/tmp', provider: 'codex', tags: ['alpha', 'beta'], start: false },
    });
    expect(sharedResp.status()).toBe(201);
    expect((await sharedResp.json()).starting, `${sharedWorker} must not autostart`).toBe(false);
    createdWorkers.push(sharedWorker);

    const sharedCardResp = await request.post('/api/board', {
      headers: ownerHeaders,
      data: {
        title: `shared-concurrent-${ts}`,
        type: 'chore',
        status: 'todo',
        session: sharedWorker,
      },
    });
    expect(sharedCardResp.status()).toBe(201);
    const sharedCard = await sharedCardResp.json();
    createdCards.push(sharedCard.id);

    // Both guests desc_append concurrently
    const [alphaAppend, betaAppend] = await Promise.all([
      alphaPage.evaluate(async (id) => {
        const r = await fetch(`/api/board/${encodeURIComponent(id)}`, {
          method: 'PATCH',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ desc_append: 'alpha concurrent note' }),
        });
        return { status: r.status, body: await r.json() };
      }, sharedCard.id),
      betaPage.evaluate(async (id) => {
        const r = await fetch(`/api/board/${encodeURIComponent(id)}`, {
          method: 'PATCH',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ desc_append: 'beta concurrent note' }),
        });
        return { status: r.status, body: await r.json() };
      }, sharedCard.id),
    ]);

    expect(alphaAppend.status).toBe(200);
    expect(betaAppend.status).toBe(200);

    // Verify both appends landed with distinct member identities in the log
    const sharedFinal = await (
      await request.get(`/api/board/${encodeURIComponent(sharedCard.id)}`, {
        headers: ownerHeaders,
      })
    ).json();
    expect(sharedFinal.desc).toContain('alpha concurrent note');
    expect(sharedFinal.desc).toContain('beta concurrent note');
    expect(sharedFinal.log).toContain('member:alpha@example.com');
    expect(sharedFinal.log).toContain('member:beta@example.com');

    // 8. Owner revokes alpha. Alpha gets 401, beta still works.
    const revokeAlpha = await request.delete(
      `/api/org/members/${encodeURIComponent(alphaMember.id)}`,
      { headers: ownerHeaders },
    );
    expect(revokeAlpha.status()).toBe(200);

    const alphaAfterRevoke = await alphaPage.evaluate(async () =>
      (await fetch('/api/identity')).status,
    );
    expect(alphaAfterRevoke).toBe(401);

    // Beta is unaffected
    const betaAfterRevoke = await betaPage.evaluate(async () => {
      const r = await fetch('/api/identity');
      return { status: r.status, body: await r.json() };
    });
    expect(betaAfterRevoke.status).toBe(200);
    expect(betaAfterRevoke.body.email).toBe('beta@example.com');

    // Alpha's full reload still shows 401 (cookie survives but member is gone)
    await alphaPage.reload();
    await alphaPage.waitForFunction(() => '_AMUX_AUTH_TOKEN' in window);
    expect(await alphaPage.evaluate(() => (window as any)._AMUX_AUTH_TOKEN)).toBe('');
    const alphaReload = await alphaPage.evaluate(async () =>
      (await fetch('/api/identity')).status,
    );
    expect(alphaReload).toBe(401);
  } finally {
    // 9. Cleanup: delete cards, workers, revoke remaining member, close contexts
    for (const card of createdCards) {
      await request
        .delete(`/api/board/${encodeURIComponent(card)}`, { headers: ownerHeaders })
        .catch(() => {});
    }
    for (const worker of createdWorkers) {
      await request
        .delete(`/api/sessions/${encodeURIComponent(worker)}`, {
          headers: { ...ownerHeaders, 'X-Amux-UI-Token': ownerUiToken },
        })
        .catch(() => {});
    }
    const remainingMembers = await (
      await request.get('/api/org/members', { headers: ownerHeaders })
    ).json();
    for (const email of ['alpha@example.com', 'beta@example.com']) {
      const m = remainingMembers.find((entry: any) => entry.email === email);
      if (m) {
        await request
          .delete(`/api/org/members/${encodeURIComponent(m.id)}`, { headers: ownerHeaders })
          .catch(() => {});
      }
    }
    // Delete invite tokens too
    for (const token of [alphaInvite.token, betaInvite.token]) {
      await request
        .delete(`/api/org/invites/${encodeURIComponent(token)}`, { headers: ownerHeaders })
        .catch(() => {});
    }
    await alphaCtx.close();
    await betaCtx.close();
  }
});
