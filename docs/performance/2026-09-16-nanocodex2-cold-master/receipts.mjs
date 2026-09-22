import { readFile, writeFile } from 'node:fs/promises';
export async function deployments() {
  const config = await readFile('/Users/georgios/Library/Preferences/.wrangler/config/default.toml', 'utf8');
  const token = /^oauth_token\s*=\s*"([^"]+)"/m.exec(config)?.[1];
  if (!token) throw Error('Wrangler credentials unavailable');
  const entries = await Promise.all(['nanocodex-durable-agent', 'nanocodex-egress', 'nanocodex', 'nanocodex-connect-api'].map(async name => {
    const r = await fetch(`https://api.cloudflare.com/client/v4/accounts/16ce0442a940f01beefdb15a196a43ea/workers/scripts/${name}/deployments`, { headers: {authorization:'Bearer '+token}, signal:AbortSignal.timeout(30000) });
    const d = await r.json();
    if (!d.success) throw Error('Deployment receipt unavailable for '+name);
    return [name,d.result.deployments[0]];
  }));
  return Object.fromEntries(entries);
}
if (process.argv[1]?.endsWith('/receipts.mjs') && process.argv[2]) {
  const result = await deployments();
  await writeFile(new URL(process.argv[2],import.meta.url),JSON.stringify(result,null,2));
  console.log(Object.fromEntries(Object.entries(result).map(([name,d])=>[name,{versions:d.versions,message:d.annotations?.['workers/message']}])));
}
