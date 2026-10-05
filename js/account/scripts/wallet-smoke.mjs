// Run: corepack pnpm --filter nanocodex-web test:wallet
// Uses Playwright Chromium by default; set WALLET_BROWSER_CHANNEL=chrome for installed Chrome.
// Screenshots and result: output/wallet-ui/ at the repository root.
// Failure cases: unavailable funding must preserve wallet identity/balance;
// checkout must preserve balance; clipboard denial must leave selectable address.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readdirSync, mkdirSync, writeFileSync } from 'node:fs';
const packages = new URL('../../../node_modules/.pnpm/', import.meta.url);
const esbuildEntry = readdirSync(packages).find(name => /^esbuild@/.test(name));
const { build } = await import(new URL(`${esbuildEntry}/node_modules/esbuild/lib/main.js`, packages));
const entry = readdirSync(packages).find(name => /^playwright-core@/.test(name));
const { chromium } = await import(new URL(`${entry}/node_modules/playwright-core/index.mjs`, packages));
const address = '0x1111111111111111111111111111111111111111';
const bundle = await build({ loader: { ".png": "dataurl" }, stdin: { contents: `
import React from 'react'; import {createRoot} from 'react-dom/client';
import {TempoWalletConnectionCard} from './src/TempoWalletConnectionCard';
import {useWalletFunding} from './src/useWalletFunding';
import {AccountSessionProvider} from './src/AccountSession';
import {QueryClient,QueryClientProvider} from '@tanstack/react-query';
const client = new QueryClient(); client.setQueryData(['session'],{account:{id:'synthetic-wallet',address:'${address}',persistent:true},reauthenticationRequired:false});
function RetryFixture(){const funding=useWalletFunding(true);return <TempoWalletConnectionCard address='${address}' balance='$5.00' fundingAmountCents={funding.amountCents} fundingAvailable={funding.available} fundingLoading={funding.loading} fundingError={funding.error} fundingErrorSource={funding.errorSource} fundingOperation={funding.operation} onFund={funding.fund}/>;}
import './src/index.css'; import '../nanocodex-connect-ui/styles.css'; import './src/DeviceConnect.css';
const state = new URLSearchParams(location.search).get('state');
function Fixture() { const [address,setAddress] = React.useState('${address}'); return <><button onClick={()=>setAddress('0x2222222222222222222222222222222222222222')}>Switch account</button><div className="device-connect-route"><div className="connect-onboarding connect-wizard"><div className="wizard-page"><div className="wizard-connectors"><TempoWalletConnectionCard address={address} balance="$5.00" fundingAmountCents={500} fundingAvailable={state === 'ready'} fundingLoading={state === 'loading'} fundingError={state === 'error' ? 'Not found' : null} fundingOperation={state === 'checkout' ? 'prepare' : null} onFund={()=>{window.funded=true}} /></div></div></div></div></>; } createRoot(document.getElementById('root')).render(state === 'retry' ? <QueryClientProvider client={client}><AccountSessionProvider><RetryFixture/></AccountSessionProvider></QueryClientProvider> : <Fixture/>);
`, resolveDir: new URL('..', import.meta.url).pathname, loader:'tsx' }, bundle:true, external:['/paradigm-mark.svg'], alias:{'nanocodex-connect-ui/ConnectionLogo':new URL('../../nanocodex-connect-ui/src/ConnectionLogo.tsx',import.meta.url).pathname}, write:false, outfile:'app.js', jsx:'automatic' });
const server = createServer((_req,res) => { res.setHeader('Content-Type','text/html'); res.end(`<meta name="viewport" content="width=device-width,initial-scale=1"><div id="root"></div><style>${bundle.outputFiles.find(f=>f.path.endsWith('.css')).text}</style><script>${bundle.outputFiles.find(f=>f.path.endsWith('.js')).text}</script>`); });
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const browser = await chromium.launch({headless:true, ...(process.env.WALLET_BROWSER_CHANNEL ? {channel:process.env.WALLET_BROWSER_CHANNEL} : {})});
const output = new URL('../../../output/wallet-ui/', import.meta.url); mkdirSync(output,{recursive:true});
try {
  for (const width of [1200,390]) {
    const page = await browser.newPage({viewport:{width,height:850}});
    await page.addInitScript(() => Object.defineProperty(navigator, 'clipboard', {configurable:true,value:{writeText:async text=>{window.copiedAddress=text}}}));
    for (const state of ['error','disabled','loading','checkout','ready']) {
      await page.goto(`http://127.0.0.1:${server.address().port}/?state=${state}`);
      await page.getByText(address,{exact:true}).waitFor();
      assert.match(await page.locator('#wallet').innerText(), /\$5\.00/);
      assert.doesNotMatch(await page.locator('#wallet').innerText(), /Not found|Onramp unavailable/i);
      await page.getByRole('button',{name:'Copy wallet address',exact:true}).click();
      assert.equal(await page.evaluate(()=>window.copiedAddress),address);
      if (state === 'ready') {
        await page.getByRole('button',{name:'Add $5.00',exact:true}).click();
        assert.equal(await page.evaluate(()=>window.funded),true);
      } else if (state !== 'checkout') assert.equal(await page.getByRole('button',{name:'Add $5.00',exact:true}).isDisabled(),true);
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth <= innerWidth),true);
      await page.screenshot({path:new URL(`${width}-${state}.png`,output).pathname});
    }
    await page.getByRole('button',{name:'Switch account',exact:true}).click();
    assert.equal(await page.getByText('Address copied',{exact:true}).count(),0);
    await page.getByRole('button',{name:'Copy wallet address',exact:true}).click();
    assert.equal(await page.evaluate(()=>window.copiedAddress),'0x2222222222222222222222222222222222222222');
    await page.evaluate(()=>Object.defineProperty(navigator,'clipboard',{value:{writeText:async()=>{throw Error('denied')}}}));
    await page.getByRole('button',{name:'Copy wallet address',exact:true}).click();
    await page.getByRole('alert').waitFor();
    assert.equal(await page.getByText('0x2222222222222222222222222222222222222222',{exact:true}).count(),1);
    const orders=[];
    await page.route('**/v1/machine-usd/config',route=>route.fulfill({json:{min_usd_amount_cents:500,max_usd_amount_cents:10000,onramp_enabled:true,chain_id:4217,token_address:'0x20c000000000000000000000f37de3740ADec032',stripe_publishable_key:'pk_test_fixture'}}));
    await page.route('**/v1/machine-usd/orders',route=>{
      orders.push({key:route.request().headers()['idempotency-key'],body:route.request().postDataJSON()});
      return route.fulfill(orders.length===1 ? {status:503,json:{error:'Service temporarily unavailable'}} : {json:{order:{id:'synthetic-order'},payment:{checkout_url:'https://checkout.stripe.com/c/pay/cs_test_wallet_retry'}}});
    });
    await page.route('https://checkout.stripe.com/**',route=>route.fulfill({contentType:'text/html',body:'Synthetic checkout reached'}));
    await page.goto(`http://127.0.0.1:${server.address().port}/?state=retry`);
    const addFunds = page.getByRole('button',{name:'Add $5.00',exact:true});
    await addFunds.click();
    await page.waitForFunction(()=>document.querySelector('#wallet [role="status"]') && !document.querySelector('#wallet .tempo-wallet-card-actions button').disabled);
    assert.equal(orders.length,1);
    assert.match(await page.locator('#wallet').innerText(), /\$5\.00/);
    await addFunds.click();
    await page.waitForURL('https://checkout.stripe.com/c/pay/cs_test_wallet_retry');
    assert.equal(orders.length,2);
    assert.notEqual(orders[0].key,orders[1].key);
    assert.equal(orders[1].body.wallet_address,address);
    await page.screenshot({path:new URL(`${width}-retry.png`,output).pathname});
    await page.close();
  }
  writeFileSync(new URL('result.txt',output),'PASS: desktop/mobile identity, balance, funding states, copy success/failure and failed order retry to checkout. Run: WALLET_BROWSER_CHANNEL=chrome node js/account/scripts/wallet-smoke.mjs\n');
  console.log('Wallet desktop and mobile checks passed');
} finally { await browser.close(); server.close(); }
