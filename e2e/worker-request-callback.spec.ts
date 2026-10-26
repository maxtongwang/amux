// Worker ownership fixtures — exercise API authorization without provider startup (refs ai-config #344).

// Worker board ownership is an authorization boundary. Peer collaboration is
// represented by reviewer/shepherd and evidence references. Another worker's
// board cannot become an execution prerequisite or receive this worker's tasks.
import { test, expect } from './fixtures';
import { createMockWorker, deleteWorkerApi } from './lifecycle/evidence';
import { cleanup } from './teardown';

test('a worker creates only on its own board and links peers explicitly', async ({ page, request }, testInfo) => {
  await page.goto('/');
  const token = await page.evaluate(() => (window as any)._AMUX_AUTH_TOKEN as string);
  expect(token, 'served bootstrap must provide the API token').toBeTruthy();
  const auth = { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' };
  const suffix = `${testInfo.project.name}-${Date.now()}`;
  const owner = `board-owner-${suffix}`;
  const reviewer = `board-reviewer-${suffix}`;
  const shepherd = `board-shepherd-${suffix}`;
  let dependency = '';
  let owned = '';

  try {
    for (const name of [owner, reviewer, shepherd]) {
      await createMockWorker(request, auth, name, { tags: ['e2e-board-ownership'] });
    }

    // Administrative setup creates a real task on the reviewer's board. The
    // worker may reference it, but cannot depend on it or create cards there.
    const dependencyMade = await request.post('/api/board', {
      headers: auth,
      data: {
        title: 'Peer-owned prerequisite', status: 'backlog', session: reviewer, type: 'chore',
      },
    });
    expect(dependencyMade.status()).toBe(201);
    dependency = (await dependencyMade.json()).id;

    const refused = await request.post('/api/board', {
      headers: { ...auth, 'X-Amux-Worker': owner },
      data: {
        title: 'Cross-board placement must fail', status: 'backlog', session: reviewer,
      },
    });
    expect(refused.status()).toBe(403);
    expect(await refused.json()).toMatchObject({
      code: 'cross_board_create_forbidden', caller: owner, requested_owner: reviewer,
    });

    const foreignDependency = await request.post('/api/board', {
      headers: { ...auth, 'X-Amux-Worker': owner },
      data: {
        title: 'Foreign dependency must fail', status: 'backlog', session: owner,
        type: 'chore', depends_on: [dependency],
      },
    });
    expect(foreignDependency.status()).toBe(409);
    expect(await foreignDependency.json()).toMatchObject({
      code: 'cross_board_dependency_forbidden',
      dependencies: [{ id: dependency, session: reviewer }],
    });

    const made = await request.post('/api/board', {
      headers: { ...auth, 'X-Amux-Worker': owner },
      data: {
        title: 'Owner-controlled work with peer links',
        desc: `Ownership stays local. Reference peer artifact ${dependency}; own any missing implementation.`,
        status: 'backlog',
        session: owner,
        type: 'chore',
        reviewer,
        shepherd,
        depends_on: [],
      },
    });
    expect(made.status()).toBe(201);
    const card = await made.json();
    owned = card.id;
    expect(card).toMatchObject({
      session: owner,
      reviewer,
      shepherd,
      depends_on: [],
    });

    const reassignRefused = await request.patch(`/api/board/${encodeURIComponent(owned)}`, {
      headers: { ...auth, 'X-Amux-Worker': owner },
      data: { session: reviewer },
    });
    expect(reassignRefused.status()).toBe(403);
    expect(await reassignRefused.json()).toMatchObject({
      code: 'cross_board_reassignment_forbidden', caller: owner, requested_owner: reviewer,
    });
    const stillOwned = await request.get(`/api/board/${encodeURIComponent(owned)}`, { headers: auth });
    expect(stillOwned.ok()).toBeTruthy();
    expect(await stillOwned.json()).toMatchObject({ session: owner, reviewer, shepherd });

    const all = await request.get('/api/board?all=1', { headers: auth });
    expect(all.ok()).toBeTruthy();
    expect((await all.json()).filter((row: any) =>
      row.title === 'Cross-board placement must fail' && row.session === reviewer,
    )).toHaveLength(0);

    // Operator-facing proof: the shipped detail view resolves the same card
    // and exposes its peer reviewer without changing the owner lane.
    await page.goto(`/#issue=${encodeURIComponent(owned)}`);
    await expect(page.locator('#board-detail-overlay')).toHaveClass(/active/, { timeout: 30_000 });
    await expect(page.locator('#bd-key')).toHaveText(owned);
    await expect(page.locator('#bd-meta')).toContainText(reviewer);
  } finally {
    if (owned) {
      await cleanup(`delete card ${owned} for ${owner}`, async () => {
        const response = await request.delete(`/api/board/${encodeURIComponent(owned)}`, { headers: auth });
        expect(response.ok(), `delete card ${owned} for ${owner}: status=${response.status()} body=${await response.text()}`).toBeTruthy();
      }, testInfo);
    }
    if (dependency) {
      await cleanup(`delete card ${dependency} for ${reviewer}`, async () => {
        const response = await request.delete(`/api/board/${encodeURIComponent(dependency)}`, { headers: auth });
        expect(response.ok(), `delete card ${dependency} for ${reviewer}: status=${response.status()} body=${await response.text()}`).toBeTruthy();
      }, testInfo);
    }
    for (const name of [owner, reviewer, shepherd]) {
      await cleanup(`delete worker for ${name}`, async () => {
        await deleteWorkerApi(page, request, auth, name);
      }, testInfo);
    }
  }
});

test('a worker cannot create an unassigned card to escape board ownership', async ({ page, request }, testInfo) => {
  await page.goto('/');
  const token = await page.evaluate(() => (window as any)._AMUX_AUTH_TOKEN as string);
  const auth = { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' };
  const owner = `board-owner-null-${testInfo.project.name}-${Date.now()}`;

  try {
    await createMockWorker(request, auth, owner, { tags: ['e2e-board-ownership'] });
    const refused = await request.post('/api/board', {
      headers: { ...auth, 'X-Amux-Worker': owner },
      data: { title: 'Unassigned escape must fail', session: null, status: 'backlog' },
    });
    expect(refused.status()).toBe(403);
    expect(await refused.json()).toMatchObject({
      code: 'cross_board_create_forbidden', caller: owner, requested_owner: null,
    });
  } finally {
    await cleanup(`delete worker for ${owner}`, async () => {
      await deleteWorkerApi(page, request, auth, owner);
    }, testInfo);
  }
});
