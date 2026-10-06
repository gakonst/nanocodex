// Run: node js/account/scripts/wallet-link-journey.mjs
// Production card, session, active-wallet, link and funding hooks; synthetic HTTP boundaries only.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdirSync, writeFileSync } from 'node:fs';
import { build } from 'esbuild';
import { chromium } from 'playwright-core';
const original = '0x1111111111111111111111111111111111111111';
const linked = '0x2222222222222222222222222222222222222222';
const token = '0x20c000000000000000000000f37de3740adec032';
const output = new URL('../../../output/wallet-link-ui/', import.meta.url);
mkdirSync(output, {recursive:true});
const bundle = await build({stdin:{contents:`
import React from 'react'; import {createRoot} from 'react-dom/client';
import {ActiveTempoWalletConnectionCard} from './src/TempoWalletConnectionCard';
import {AccountSessionProvider,useAccountSession} from './src/AccountSession';
import {QueryClient,QueryClientProvider} from '@tanstack/react-query';
import './src/index.css'; import '../nanocodex-connect-ui/styles.css'; import './src/DeviceConnect.css';
const client=new QueryClient({defaultOptions:{queries:{retry:false}}});
function App(){const session=useAccountSession();return <><output aria-label="Sign-in address">{session.account?.address}</output><button onClick={()=>session.refresh()}>Refresh session</button><div className="device-connect-route"><div className="connect-onboarding connect-wizard"><div className="wizard-page"><div className="wizard-connectors">{session.account ? <ActiveTempoWalletConnectionCard key={session.account.id} enabled={true}/> : null}</div></div></div></div></>}
createRoot(document.getElementById('root')).render(<QueryClientProvider client={client}><AccountSessionProvider><App/></AccountSessionProvider></QueryClientProvider>);
`, resolveDir:new URL('..',import.meta.url).pathname,loader:'tsx'},bundle:true,external:['/paradigm-mark.svg'],alias:{'nanocodex-connect-ui/ConnectionLogo':new URL('../../nanocodex-connect-ui/src/ConnectionLogo.tsx',import.meta.url).pathname},write:false,outfile:'app.js',jsx:'automatic'});
const server=createServer((_req,res)=>{res.setHeader('Content-Type','text/html');res.end(`<meta name="viewport" content="width=device-width,initial-scale=1"><div id="root"></div><style>${bundle.outputFiles.find(f=>f.path.endsWith('.css')).text}</style><script>${bundle.outputFiles.find(f=>f.path.endsWith('.js')).text}</script>`)});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const browser=await chromium.launch({headless:true,...(process.env.WALLET_BROWSER_EXECUTABLE ? {executablePath:process.env.WALLET_BROWSER_EXECUTABLE}:{}),...(process.env.WALLET_BROWSER_CHANNEL ? {channel:process.env.WALLET_BROWSER_CHANNEL}:{})});
const evidence=[];
try {
  for(const width of [1200,390]) {
    const context=await browser.newContext({viewport:{width,height:900}});
    await context.tracing.start({screenshots:true,snapshots:true,sources:true});
    const page=await context.newPage();
    const errors=[];page.on('pageerror',error=>errors.push(error.message));
    let account='11111111-1111-4111-8111-111111111111'; let active=original; let status='pending'; let invalidOrigin=false; let unknownCreate=false;
    let unknownCancel=false; let invalidGrant=false; let promptReady=false; let wrongBalance=false; let linkReads=0; let pollReads=0; const calls=[]; const orders=[]; const operations=new Map();
    const wallet=()=>({address:active,mode:active===original?'internal':'linked',original_address:original,...(active===linked?{access_key:{address:'0x3333333333333333333333333333333333333333',expiry:null,permissions:invalidGrant?'limited':'full'}}:{})});
    await context.route('**/v1/**', async route=>{
      const req=route.request(); const path=new URL(req.url()).pathname;
      const body=req.method()==='POST'?req.postDataJSON():undefined;
      calls.push({path,body,account});
      if(path==='/v1/me') return route.fulfill({json:{user:{id:account,address:original,persistent:true}}});
      if(path==='/v1/wallet') return route.fulfill({json:wallet()});
      if(path==='/v1/wallet/balance') return route.fulfill({json:{account:wrongBalance?original:active,balance:active===original?'5000000':'19000000',decimals:6,symbol:'MACH',token}});
      if(path==='/v1/machine-usd/config') return route.fulfill({json:{min_usd_amount_cents:500,max_usd_amount_cents:10000,onramp_enabled:true,chain_id:4217,token_address:token,stripe_publishable_key:'pk_test_fixture'}});
      if(path==='/v1/machine-usd/orders') {
        orders.push({key:req.headers()['idempotency-key'],body});
        return route.fulfill({json:{order:{id:'ord_11111111111111111111111111111111',wallet_address:body.wallet_address,usd_amount_cents:500,mach_amount_atomics:5000000},payment:{provider:'stripe',mode:'hosted_checkout',checkout_url:'https://checkout.stripe.com/c/pay/cs_test_linked'}}});
      }
      if(path.startsWith('/v1/machine-usd/orders/')) return route.fulfill({json:{order:{id:'ord_11111111111111111111111111111111',wallet_address:orders.at(-1).body.wallet_address,usd_amount_cents:500,mach_amount_atomics:5000000,status:'requires_payment'}}});
      if(path==='/v1/wallet/unlink') {assert.equal(body.expected_address,linked);active=original;return route.fulfill({json:{operation_id:body.operation_id,status:'unlinked'}})}
      if(path==='/v1/wallet/link/cancel') {if(unknownCancel){unknownCancel=false;return route.fulfill({status:503,json:{error:'Cancellation receipt unavailable'}})} operations.set(body.operation_id,'cancelled');return route.fulfill({json:{operation_id:body.operation_id,status:'cancelled'}})}
      if(path==='/v1/wallet/link'||path==='/v1/wallet/link/poll') {
        if(path.endsWith('/poll')) {pollReads++;promptReady=true;} else linkReads++;
        if(unknownCreate) {unknownCreate=false;return route.fulfill({status:503,json:{error:'Connection interrupted'}})}
        const result=operations.get(body.operation_id)??status;
        if(result==='linked') active=linked;
        return route.fulfill({json:{operation_id:body.operation_id,status:result,...(result==='pending'&&promptReady?{approval_url:(invalidOrigin?'https://untrusted.example':'https://wallet.tempo.xyz')+'/connect?request=synthetic',user_code:'TEST-CODE',expires_at:Date.now()+600000}:{})}});
      }
      return route.fulfill({status:404,json:{error:'Unexpected synthetic endpoint: '+path}});
    });
    await context.route('https://wallet.tempo.xyz/**',route=>route.fulfill({contentType:'text/html',body:'Synthetic Tempo approval boundary. No actual permission granted.'}));
    const begin=async()=>{await page.getByRole('button',{name:'Link Tempo Wallet',exact:true}).click();assert.equal(await page.getByRole('link',{name:'Open Tempo Wallet'}).count(),0);await page.getByText(/spend without limits and make all contract calls/).waitFor();await page.getByRole('button',{name:'Continue to Tempo Wallet'}).click()};
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    await page.getByText('Balance: $5.00',{exact:true}).waitFor();
    assert.equal(linkReads,0);
    await begin();
    await page.getByText('Preparing the Tempo Wallet approval…',{exact:true}).waitFor();
    const approval=page.getByRole('link',{name:'Open Tempo Wallet',exact:true});await approval.waitFor();
    const [popup]=await Promise.all([page.waitForEvent('popup'),approval.click()]);await popup.waitForURL('https://wallet.tempo.xyz/connect?request=synthetic');await popup.close();
    await page.getByText(/Closing its tab does not cancel/).waitFor();
    const first=calls.find(call=>call.path==='/v1/wallet/link').body.operation_id;
    await page.reload();await approval.waitFor();
    assert.equal(calls.filter(call=>call.path==='/v1/wallet/link').at(-1).body.operation_id,first);
    assert.equal(await page.getByText(/Approved access: unlimited spending/).count(),0);
    assert.equal(await page.getByLabel('Sign-in address').innerText(),original);
    invalidGrant=true;status='linked';
    await page.getByText('Couldn’t verify the linked wallet permissions.',{exact:true}).waitFor();
    assert.equal(await page.getByText(/Approved access: unlimited spending/).count(),0);
    assert.equal(await page.getByRole('button',{name:'Add $5.00',exact:true}).isDisabled(),true);
    assert.equal(await page.getByText(/Tempo Wallet linked. Funding/).count(),0);
    invalidGrant=false;wrongBalance=true;
    await page.getByRole('button',{name:'Resume wallet request',exact:true}).click();
    await page.locator('#wallet code').filter({hasText:linked}).waitFor();
    await page.getByText('Balance: Balance unavailable',{exact:true}).waitFor();
    assert.doesNotMatch(await page.locator('#wallet').innerText(),/Balance: \$5\.00/);
    assert.equal(await page.getByLabel('Sign-in address').innerText(),original);
    assert.equal(await page.evaluate(()=>Object.keys(localStorage).filter(k=>k.startsWith('nanocodex:wallet-link:')).length),0);
    wrongBalance=false;await page.reload();await page.getByText('Balance: $19.00',{exact:true}).waitFor();
    await page.screenshot({path:new URL(`${width}-linked.png`,output).pathname});
    // Actual production funding must bind both request and saved pending order to the linked address.
    await page.getByRole('button',{name:'Add $5.00',exact:true}).click();await page.getByRole('link',{name:'Open Stripe checkout'}).waitFor();
    assert.equal(orders.at(-1).body.wallet_address,linked);
    assert.ok(await page.evaluate(address=>sessionStorage.getItem('nanocodex:mach-funding:11111111-1111-4111-8111-111111111111:'+address),linked));
    // Reload leaves the pending order recoverable, while unlink is a separate explicit choice.
    await page.reload();await page.getByRole('button',{name:'Unlink Tempo Wallet',exact:true}).click();
    await page.getByText(/does not revoke the onchain access key/).waitFor();
    await page.getByRole('button',{name:'Disconnect linked wallet',exact:true}).click();
    await page.locator('#wallet code').filter({hasText:original}).waitFor();await page.getByText('Balance: $5.00',{exact:true}).waitFor();
    assert.equal(await page.getByLabel('Sign-in address').innerText(),original);
    assert.ok(await page.evaluate(address=>sessionStorage.getItem('nanocodex:mach-funding:11111111-1111-4111-8111-111111111111:'+address),linked));
    await page.getByRole('button',{name:'Add $5.00',exact:true}).click();await page.getByRole('link',{name:'Open Stripe checkout'}).waitFor();
    assert.equal(orders.at(-1).body.wallet_address,original);assert.notEqual(orders[0].key,orders[1].key);
    await page.reload();
    // Reject/expire/cancel remain explicit and do not switch addresses.
    for(const terminal of ['rejected','expired']) {
      status=terminal; await begin(); await page.getByText(`Wallet link ${terminal}. Your active wallet is unchanged.`,{exact:true}).waitFor();
      assert.equal(await page.locator('#wallet code').innerText(),original);
    }
    status='pending';await begin();await approval.waitFor();await page.getByRole('button',{name:'Cancel wallet link',exact:true}).click();
    await page.getByText('Wallet link cancelled. Your active wallet is unchanged.',{exact:true}).waitFor();
    // A lost cancellation response must replay cancellation, never restart approval.
    status='pending';await begin();await approval.waitFor();
    const cancelId=calls.filter(call=>call.path==='/v1/wallet/link').at(-1).body.operation_id;
    unknownCancel=true;await page.getByRole('button',{name:'Cancel wallet link',exact:true}).click();
    await page.getByRole('button',{name:'Resume wallet request',exact:true}).waitFor();
    const startsBeforeCancelRecovery=linkReads;
    await page.reload();await page.getByText('Wallet link cancelled. Your active wallet is unchanged.',{exact:true}).waitFor();
    assert.equal(linkReads,startsBeforeCancelRecovery);
    assert.equal(calls.filter(call=>call.path==='/v1/wallet/link/cancel').at(-1).body.operation_id,cancelId);
    status='interrupted';await begin();await page.getByText(/wallet request was interrupted/).waitFor();
    const interrupted=calls.filter(call=>call.path==='/v1/wallet/link').at(-1).body.operation_id;
    status='pending';await begin();await approval.waitFor();
    assert.notEqual(calls.filter(call=>call.path==='/v1/wallet/link').at(-1).body.operation_id,interrupted);
    await page.getByRole('button',{name:'Cancel wallet link',exact:true}).click();await page.getByRole('button',{name:'Link Tempo Wallet',exact:true}).waitFor();
    unknownCreate=true;await begin();await page.getByRole('button',{name:'Resume wallet request',exact:true}).waitFor();
    const unknown=calls.filter(call=>call.path==='/v1/wallet/link').at(-1).body.operation_id;
    await page.reload();await approval.waitFor();assert.equal(calls.filter(call=>call.path==='/v1/wallet/link').at(-1).body.operation_id,unknown);
    // Identity switch must not recover another account's request.
    const readsBefore=calls.filter(call=>call.path.includes('/wallet/link')).length;
    account='22222222-2222-4222-8222-222222222222';await page.getByRole('button',{name:'Refresh session',exact:true}).click();
    await page.getByRole('button',{name:'Link Tempo Wallet',exact:true}).waitFor();
    assert.equal(calls.filter(call=>call.path.includes('/wallet/link')).length,readsBefore);
    assert.equal(await approval.count(),0);
    account='11111111-1111-4111-8111-111111111111';await page.getByRole('button',{name:'Refresh session',exact:true}).click();await approval.waitFor();
    assert.equal(calls.filter(call=>call.path==='/v1/wallet/link').at(-1).body.operation_id,unknown);
    await page.getByRole('button',{name:'Cancel wallet link',exact:true}).click();await page.getByRole('button',{name:'Link Tempo Wallet',exact:true}).waitFor();
    invalidOrigin=true;await begin();await page.getByText('The wallet approval destination is invalid. Cancel this request.',{exact:true}).waitFor();assert.equal(await approval.count(),0);
    await page.getByRole('button',{name:'Cancel wallet link',exact:true}).click();await page.getByRole('button',{name:'Link Tempo Wallet',exact:true}).waitFor();
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
    assert.deepEqual(errors,[]);assert.ok(pollReads>0);
    await page.screenshot({path:new URL(`${width}-recovered.png`,output).pathname});
    await context.tracing.stop({path:new URL(`${width}-trace.zip`,output).pathname});
    evidence.push({width,result:'PASS',calls,orders});await context.close();
  }
  writeFileSync(new URL('result.json',output),JSON.stringify({command:'node js/account/scripts/wallet-link-journey.mjs',expected:'Explicit full-access consent; fixed approval origin; reload/unknown-result recovery; original sign-in preserved; active-wallet balance validation and funding; local unlink; rejection, expiry, cancel, interruption and identity separation; no grant claims for limited access; lost cancellation replays cancel',observed:evidence},null,2));
  console.log('PASS: desktop/mobile production wallet linking, recovery, identity, balance, funding, unlink and origin validation');
} finally {await browser.close();server.close()}
