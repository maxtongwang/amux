import {test, expect} from '../operands';
import {boot, auth, checkpoint} from './evidence';

test('LC-SYNC-PROGRESS: reconnect checks off only acknowledged changes and keeps failures reviewable', async ({page,request,context,operands},info)=>{
  // F03/F12 operands: what the pill's precedence reads when it says Sync error.
  const ops=operands({label:'sync-progress', paths:['/api/sessions','/api/board','/api/sync'],
    globals:['_sessionLoadError','_boardReadError','_syncReadError','_localWriteError','_writeError','online','navigator.onLine',
      '_syncPillText','_liveSSE','_sseGraceUntil','consecutiveFailures','offlineQueue','_syncChecklist','peekSession'],
    dom:['#conn-status','#sync-banner','#sync-title-text','#sync-items .sync-item']});
  test.setTimeout(90_000); await boot(page); const headers=await auth(page);
  const cards:any[]=[]; const releases:Array<()=>void>=[]; let allowConflict=false;
  const queue=()=>page.evaluate(()=>JSON.parse(localStorage.getItem('amux_offline_queue')||'[]'));
  try {
    for(let i=0;i<3;i++) {
      const r=await request.post('/api/board',{headers,data:{title:`Sync step ${i} ${info.project.name}`,type:'chore'}});
      expect(r.ok()).toBe(true); cards.push(await r.json());
      await page.route(`**/api/board/${cards[i].id}`,async route=>{
        if(route.request().method()!=='PATCH') return route.continue();
        if(i===1 && !allowConflict) return route.fulfill({status:409,json:{error:'revision conflict during reconnect'}});
        await new Promise<void>(resolve=>{releases[i]=resolve});
        await route.continue();
      });
    }
    ops.mark('offline'); await context.setOffline(true); await page.evaluate(()=>window.dispatchEvent(new Event('offline')));
    for(const card of cards) {
      const status=await page.evaluate(async c=>(await fetch('/api/board/'+c.id,{method:'PATCH',headers:{'Content-Type':'application/json'},body:JSON.stringify({title:'Reconnected '+c.id,expect_rev:c.rev})})).status,card);
      expect(status).toBe(202);
    }
    expect(await queue()).toHaveLength(3);
    ops.mark('online'); await context.setOffline(false); await page.evaluate(()=>window.dispatchEvent(new Event('online')));
    await expect.poll(()=>Boolean(releases[0])).toBe(true);
    // A reconnect with a backlog opens the checklist on the home page, and the
    // pill carries the same progress (Ethan, 2026-09-24).
    await expect(page.locator('#sync-banner')).toHaveClass(/active/);
    await expect(page.locator('#conn-status')).toHaveText(/Syncing 0\/3/);
    await expect(page.locator('#toast')).not.toHaveClass(/visible/);
    await expect(page.locator('#sync-items .running')).toHaveCount(1);
    await expect(page.locator('#sync-items .done')).toHaveCount(0);
    await checkpoint(page,info,'sync-before-acknowledgement');
    releases[0](); await expect.poll(()=>Boolean(releases[2])).toBe(true);
    await expect(page.locator('#sync-items .done')).toHaveCount(1);
    await expect(page.locator('#sync-items .failed')).toHaveCount(1);
    await expect(page.locator('#sync-items .running')).toHaveCount(1);
    await expect.poll(()=>page.locator('#toast').evaluate(el=>getComputedStyle(el).opacity)).toBe('0');
    await checkpoint(page,info,'sync-individual-checkmarks');
    releases[2](); await expect(page.locator('#sync-title-text')).toHaveText('2 synced, 1 failed');
    await expect(page.locator('#sync-items .done')).toHaveCount(2);
    await expect(page.locator('#sync-items .done').first()).toContainText('✔');
    await expect(page.locator('#sync-banner')).toHaveClass(/active/);
    expect((await queue()).map((q:any)=>q.state)).toEqual(['blocked']);
    for(const [i,c] of cards.entries()) {
      const read=await (await request.get('/api/board/'+c.id,{headers})).json();
      expect(read.title).toBe(i===1?c.title:'Reconnected '+c.id);
    }
    allowConflict=true;
    // Failed changes are retried from the status modal (07b42586): tap the
    // "N failed" pill, then Retry now.
    await page.locator('#conn-status').click();
    await page.locator('#conn-retry-now').click();
    await expect.poll(()=>Boolean(releases[1])).toBe(true);
    await expect(page.locator('#sync-items .done')).toHaveCount(2); // earlier acknowledgements remain visible
    releases[1](); await expect.poll(async()=>(await queue()).length).toBe(0);
    await expect(page.locator('#sync-title-text')).toHaveText('3 synced');
    await expect(page.locator('#sync-items .done')).toHaveCount(3);
    await checkpoint(page,info,'sync-failed-step-recovered');
  } finally {
    await ops.capture('finally'); // before the releases below change the state
    for(const release of releases) release?.(); await context.setOffline(false);
    await page.evaluate(async ids=>(window as any)._mutateQueue((q:any[])=>{for(let i=q.length-1;i>=0;i--)if(ids.some(id=>q[i].url==='/api/board/'+id))q.splice(i,1)}),cards.map(c=>c.id));
    for(const c of cards) await request.delete('/api/board/'+c.id,{headers});
  }
});
