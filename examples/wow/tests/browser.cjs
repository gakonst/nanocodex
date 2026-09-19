const assert = require('node:assert/strict');
const { chromium } = require('playwright');
(async () => {
  const browser = await chromium.launch({executablePath:process.env.CHROMIUM || '/usr/bin/chromium', headless:true, args:['--no-sandbox']});
  const page = await browser.newPage({viewport:{width:1440,height:1000}});
  const errors=[], sends=[]; let gameReads=0;
  page.on('pageerror', e=>errors.push(e.message));
  await page.route('**/api/**', async route=>{
    const url=new URL(route.request().url()); let result;
    switch(url.pathname){
      case '/api/status': result={connected:true,auth:'cli-account-store',voice:{available:false}};break;
      case '/api/thread': result={state:{active_turns:[]}};break;
      case '/api/projects': result={projects:[{id:'root',name:'Work project'}]};break;
      case '/api/threads': result={threads:[{id:'work-agent',title:'Build task',status:'unknown'}]};break;
      case '/api/messages':
        result=url.searchParams.get('thread_id')==='game-agent'?{messages:++gameReads>=2?[{role:'user',text:'Tell me the lore'},{role:'assistant',text:'A delayed lore response <img src=x onerror=alert(1)>'}]:[{role:'user',text:'Tell me the lore'}]}:{messages:[{role:'assistant',text:'Work thread ready'}]};break;
      case '/api/send': sends.push(route.request().postDataJSON());result={thread_id:sends.at(-1).mode==='agent'?'work-agent':'game-agent',status:'accepted'};break;
      default: throw Error('Unexpected API '+url.pathname);
    }
    await route.fulfill({json:result});
  });
  await page.goto(process.env.WOW_TEST_URL || 'http://127.0.0.1:17841');
  await page.getByRole('button',{name:'Build task'}).click();
  await page.locator('#prompt').fill('Continue the implementation');
  await page.locator('#send').click();
  await page.waitForFunction(()=>!document.querySelector('#send').disabled);
  assert.equal(sends[0].thread_id,'work-agent');assert.equal(sends[0].mode,'agent');
  await page.locator('#importContext').fill(JSON.stringify({schemaVersion:1,client:{version:'12.0'},character:{class:'Mage'},specialization:{name:'Frost'},location:{zone:'Elwynn Forest'}}));
  await page.locator('#import').click();
  assert.equal(await page.locator('#spec').inputValue(),'Frost');assert.equal(await page.locator('#zone').inputValue(),'Elwynn Forest');
  await page.locator('.mode-tabs [data-mode="lore"]').click();
  await page.locator('#prompt').fill('Tell me the lore');await page.locator('#send').click();
  await page.waitForFunction(()=>document.querySelector('#messages').textContent.includes('A delayed lore response'),{},{timeout:10000});
  assert.equal(sends[1].project_id,null);assert.equal(sends[1].thread_id,null);assert.equal(sends[1].context.specialization.name,'Frost');
  assert.equal(await page.locator('#messages img').count(),0);
  await page.locator('.mode-tabs [data-mode="agent"]').click();
  await page.waitForFunction(()=>document.querySelector('#messages').textContent.includes('Work thread ready'));
  assert.deepEqual(errors,[]);
  await browser.close();console.log('PASS: project selection, work continuation, game isolation, nested addon import, delayed reply polling, text-only rendering');
})().catch(e=>{console.error(e);process.exit(1)});
