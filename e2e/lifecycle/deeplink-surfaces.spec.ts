// Deep link coverage — navigate fixtures without launching providers (refs ai-config #339).
import { test, expect } from '../fixtures';
import { boot, auth, deleteOwnedWorkers, checkpoint, createMockWorker } from './evidence';

test('LC-DEEPLINK: task-to-worker navigation closes the task overlay and preserves its edit', async ({page,request},info) => {
  await boot(page); const headers=await auth(page);
  const name=`lc-deeplink-${info.project.name}-${Date.now()}`;
  await createMockWorker(request, headers, name);
  const response=await request.post('/api/board',{headers,data:{title:'Preserve linked task draft',type:'chore',session:name}});
  expect(response.ok()).toBe(true);const card=await response.json();
  try {
    await page.goto('/#issue='+card.id);await page.reload();
    await expect(page.locator('#bd-key')).toHaveText(card.id);
    await page.locator('#bd-tab-edit').click();await page.locator('#bd-desc').fill('An unsaved task edit survives linked navigation.');
    await page.goto('/#peek='+name);
    await expect(page.locator('#peek-overlay')).toHaveClass(/active/);
    await expect(page.locator('#board-detail-overlay')).not.toHaveClass(/active/);
    await checkpoint(page,info,'linked-worker-is-unobstructed');
    await page.getByRole('button',{name:'Close worker',exact:true}).click();
    await page.goto('/#issue='+card.id);
    await expect(page.locator('#bd-key')).toHaveText(card.id);
    await page.locator('#bd-tab-edit').click();
    await expect(page.locator('#bd-desc')).toHaveValue('An unsaved task edit survives linked navigation.');
  } finally { await deleteOwnedWorkers(page,request,headers,[name]); }
});
