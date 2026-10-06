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
const bundle = await build({ stdin: { contents: `
import React from 'react'; import {createRoot} from 'react-dom/client';
import {TempoWalletConnectionCard} from './src/TempoWalletConnectionCard';
import {useWalletFunding} from './src/useWalletFunding';
import {useAccountQuery} from './src/useAccountQuery';
import {decodeWalletBalance,formatWalletBalance} from './src/walletFunding';
import {AccountSessionProvider} from './src/AccountSession';
import {QueryClient,QueryClientProvider} from '@tanstack/react-query';
const client = new QueryClient(); client.setQueryData(['session'],{account:{id:'synthetic-wallet',address:'${address}',persistent:true},reauthenticationRequired:false});
function RetryFixture(){const funding=useWalletFunding(true);const {query}=useAccountQuery('synthetic-wallet','/v1/wallet/balance',value=>decodeWalletBalance(value,'${address}'));return <TempoWalletConnectionCard address='${address}' balance={query.data ? formatWalletBalance(query.data) : 'Loading balance…'} fundingAmountCents={funding.amountCents} fundingAvailable={funding.available} fundingLoading={funding.loading} fundingError={funding.error} fundingErrorSource={funding.errorSource} checkoutUrl={funding.checkoutUrl} fundingMessage={funding.message} fundingOperation={funding.operation} onFund={funding.fund}/>;}
import './src/index.css'; import '../nanocodex-connect-ui/styles.css'; import './src/DeviceConnect.css';
const state = new URLSearchParams(location.search).get('state');
function Fixture() { const [address,setAddress] = React.useState('${address}'); return <><button onClick={()=>setAddress('0x2222222222222222222222222222222222222222')}>Switch account</button><div className="device-connect-route"><div className="connect-onboarding connect-wizard"><div className="wizard-page"><div className="wizard-connectors"><TempoWalletConnectionCard address={address} balance="$5.00" fundingAmountCents={500} fundingAvailable={state === 'ready'} fundingLoading={state === 'loading'} fundingError={state === 'error' ? 'Not found' : null} fundingOperation={state === 'checkout' ? 'prepare' : null} onFund={()=>{window.funded=true}} /></div></div></div></div></>; } createRoot(document.getElementById('root')).render(state === 'retry' ? <QueryClientProvider client={client}><AccountSessionProvider><RetryFixture/></AccountSessionProvider></QueryClientProvider> : <Fixture/>);
`, resolveDir: new URL('..', import.meta.url).pathname, loader:'tsx' }, bundle:true, external:['/paradigm-mark.svg'], alias:{'nanocodex-connect-ui/ConnectionLogo':new URL('../../nanocodex-connect-ui/src/ConnectionLogo.tsx',import.meta.url).pathname}, write:false, outfile:'app.js', jsx:'automatic' });
const server = createServer((_req,res) => { res.setHeader('Content-Type','text/html'); res.end(`<meta name="viewport" content="width=device-width,initial-scale=1"><div id="root"></div><style>${bundle.outputFiles.find(f=>f.path.endsWith('.css')).text}</style><script>${bundle.outputFiles.find(f=>f.path.endsWith('.js')).text}</script>`); });
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const browser = await chromium.launch({headless:true, ...(process.env.WALLET_BROWSER_EXECUTABLE ? {executablePath:process.env.WALLET_BROWSER_EXECUTABLE} : {}), ...(process.env.WALLET_BROWSER_CHANNEL ? {channel:process.env.WALLET_BROWSER_CHANNEL} : {})});
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
    await page.route('**/v1/wallet', route => route.fulfill({json:{address, mode:'internal', original_address:address}}));
    const orders=[];
    await page.route('**/v1/machine-usd/config',route=>route.fulfill({json:{min_usd_amount_cents:500,max_usd_amount_cents:10000,onramp_enabled:true,chain_id:4217,token_address:'0x20c000000000000000000000f37de3740ADec032',stripe_publishable_key:'pk_test_fixture'}}));
    await page.route('**/v1/machine-usd/orders',route=>{
      orders.push({key:route.request().headers()['idempotency-key'],body:route.request().postDataJSON()});
      return route.fulfill(orders.length===1 ? {status:503,json:{error:'Service temporarily unavailable'}} : {json:{order:{id:'ord_11111111111111111111111111111111',wallet_address:address,usd_amount_cents:500,mach_amount_atomics:5000000},payment:{provider:'stripe',mode:'hosted_checkout',checkout_url:'https://checkout.stripe.com/c/pay/cs_test_wallet_retry'}}});
    });
    let status = 'requires_payment';
    let statusReads = 0;
    let malformedCompletion = false;
    let balanceReads = 0;
    await page.route('**/v1/wallet/balance',route=>{
      balanceReads++;
      return route.fulfill({json:{account:address,balance:status==='complete' && !malformedCompletion ? '10000000':'5000000',decimals:6,symbol:'MACH',token:'0x20c000000000000000000000f37de3740adec032'}});
    });
    await page.route('**/v1/machine-usd/orders/*',route=>{
      statusReads++;
      return route.fulfill({json:{order:{id:'ord_11111111111111111111111111111111',wallet_address:address,usd_amount_cents:500,mach_amount_atomics:5000000,status,issuance_status:status==='complete' && !malformedCompletion ? 'fulfilled':'not_started',issuance_transaction_hash:status==='complete' ? '0x'+'a'.repeat(64):null}}});
    });
    await page.context().route('https://checkout.stripe.com/**',route=>route.fulfill({contentType:'text/html',body:'Synthetic checkout reached'}));
    await page.goto(`http://127.0.0.1:${server.address().port}/?state=retry`);
    const addFunds = page.getByRole('button',{name:'Add $5.00',exact:true});
    await addFunds.click();
    await page.waitForFunction(()=>document.querySelector('#wallet [role="status"]') && !document.querySelector('#wallet .tempo-wallet-card-actions button').disabled);
    assert.equal(orders.length,1);
    assert.match(await page.locator('#wallet').innerText(), /\$5\.00/);
    await page.reload();
    await page.getByRole('button',{name:'Add $5.00',exact:true}).click();
    const checkout = page.getByRole('link',{name:'Open Stripe checkout',exact:true});
    await checkout.waitFor();
    const [popup] = await Promise.all([page.waitForEvent('popup'), checkout.click()]);
    await popup.waitForURL('https://checkout.stripe.com/c/pay/cs_test_wallet_retry');
    assert.match(page.url(), /state=retry/);
    await popup.close();
    await page.waitForFunction(()=>document.querySelector('#wallet').innerText.includes('Keep this page open'));
    assert.equal(await page.getByText('Funds added to your Wallet.',{exact:true}).count(),0);
    assert.equal(orders.length,2);
    assert.equal(orders[0].key,orders[1].key);
    assert.deepEqual(orders[0].body,orders[1].body);
    assert.equal(orders[1].body.wallet_address,address);
    await page.screenshot({path:new URL(`${width}-retry.png`,output).pathname});
    // Reload and resume the existing capability-bound order; replay inputs stay identical.
    await page.reload();
    await page.getByRole('button',{name:'Add $5.00',exact:true}).click();
    await checkout.waitFor();
    assert.equal(orders.length,3);
    assert.deepEqual(orders[2],orders[1]);
    status = 'issuing';
    const beforePayment = statusReads;
    await page.waitForFunction(()=>document.querySelector('#wallet').innerText.includes('Keep this page open'));
    for (let tries=0;statusReads===beforePayment && tries<100;tries++) await new Promise(resolve=>setTimeout(resolve,100));
    assert.ok(statusReads>beforePayment);
    assert.equal(await page.getByText('Funds added to your Wallet.',{exact:true}).count(),0);
    // A payment receipt without fulfilled issuance must remain recoverable, never complete.
    status = 'complete'; malformedCompletion = true;
    await page.getByRole('button',{name:'Add $5.00',exact:true}).waitFor();
    assert.equal(await page.getByText('Funds added to your Wallet.',{exact:true}).count(),0);
    malformedCompletion = false;
    const beforeBalanceRefresh = balanceReads;
    await page.getByRole('button',{name:'Add $5.00',exact:true}).click();
    await page.getByText('Funds added to your Wallet.',{exact:true}).waitFor();
    assert.equal(orders.length,3);
    await page.getByText('Balance: $10.00',{exact:true}).waitFor();
    assert.ok(balanceReads>beforeBalanceRefresh);
    assert.equal(await checkout.count(),0);
    const storageKey = 'nanocodex:mach-funding:synthetic-wallet:' + address;
    assert.equal(await page.evaluate(key=>sessionStorage.getItem(key),storageKey),null);
    await page.screenshot({path:new URL(`${width}-complete.png`,output).pathname});
    // Invalid persisted state cannot dispatch a new order or reuse another account's capability.
    const validIntent = {accountId:'synthetic-wallet',address,orderToken:orders[0].body.order_token,idempotencyKey:orders[0].key,amountCents:500};
    const invalidIntents = [
      '{',
      ...[
        {accountId:'foreign-account'},
        {address:'0x2222222222222222222222222222222222222222'},
        {amountCents:'500'},
        {amountCents:499},
        {id:'unrecognized-order'},
        {orderToken:'invalid-capability'},
        {idempotencyKey:'invalid-key'},
        {checkoutUrl:'https://untrusted.example'},
      ].map(change=>JSON.stringify({...validIntent,...change})),
    ];
    for (const retained of invalidIntents) {
      await page.evaluate(([key,value])=>sessionStorage.setItem(key,value),[storageKey,retained]);
      await page.reload();
      await page.getByRole('button',{name:'Add $5.00',exact:true}).click();
      await page.getByText(/Contact support before starting another payment/).waitFor();
      assert.equal(orders.length,3);
    }
    await page.close();
  }
  writeFileSync(new URL('result.txt',output),'PASS: desktop/mobile identity, balance, funding states, copy success/failure and unknown-create identical replay, separate checkout tab, tab-close pending, reload recovery, payment-only pending, invalid issuance rejection, fulfilled completion, and malformed/foreign saved intent rejection. Run: WALLET_BROWSER_CHANNEL=chrome node js/account/scripts/wallet-smoke.mjs\n');
  console.log('Wallet desktop and mobile checks passed');
} finally { await browser.close(); server.close(); }
