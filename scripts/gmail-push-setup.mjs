#!/usr/bin/env node
import { execFileSync } from 'node:child_process';
import { isIP } from 'node:net';
import { pathToFileURL } from 'node:url';

const help = `Usage: node scripts/gmail-push-setup.mjs --project PROJECT_ID
  --gmail-oauth-project PROJECT_ID --push-endpoint https://public-host/path [--apply]

Default: offline dry-run; no gcloud calls. --apply enables the Pub/Sub API,
checks existing resources, creates missing resources, and adds scoped IAM grants.
--project must equal --gmail-oauth-project: explicitly confirm the project owning
Gmail's OAuth client (the helper cannot discover that association from a client ID).
Options:
  --topic NAME              Default gmail-push
  --subscription NAME       Default gmail-push
  --service-account NAME    Default gmail-push (account ID within --project)
  --audience HTTPS_URL      Exact OIDC audience; default is the push endpoint
  --help                    Show this help

Use a publicly reachable DNS hostname with HTTPS, without credentials, query or
fragment. Configure the receiver to verify Google's OIDC signature, issuer,
audience and service-account email. DNS reachability is not checked offline.
Caller needs permission to enable services, create resources, change their IAM,
and iam.serviceAccounts.actAs on the push account. No keys or tokens are printed.
Existing incompatible subscriptions fail; they are never updated or deleted.
Partial apply failures can be rerun after fixing permissions. No watch requests,
deployment or live mailbox registration are performed by this helper.
See https://docs.cloud.google.com/pubsub/docs/authenticate-push-subscriptions
`;

function publicHttps(value, label) {
  let u;
  try { u = new URL(value); } catch { throw new Error(`${label} must be a public HTTPS URL`); }
  const h = u.hostname.toLowerCase();
  if (u.protocol !== 'https:' || u.username || u.password || u.search || u.hash ||
      isIP(h) || h.includes(':') || !h.includes('.') ||
      /(^|\.)(localhost|local|internal|test|invalid)$/.test(h) ||
      !/^[a-z0-9.-]+$/.test(h) || h.endsWith('.')) {
    throw new Error(`${label} must be public HTTPS without credentials, query or fragment`);
  }
  return value;
}

function gcloud(args) {
  return JSON.parse(execFileSync('gcloud', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], timeout: 120000, maxBuffer: 16 * 1024 * 1024 }) || '{}');
}

export function setup(argv, { run = gcloud, log = console.log } = {}) {
  if (argv.length === 1 && argv[0] === '--help') { log(help); return; }
  const options = {}, known = new Set(['project', 'gmail-oauth-project', 'push-endpoint', 'topic', 'subscription', 'service-account', 'audience']);
  for (let i = 0; i < argv.length; i++) {
    const key = argv[i].replace(/^--/, '');
    if (!argv[i].startsWith('--') || Object.hasOwn(options, key)) throw new Error('Unknown or duplicate option');
    if (key === 'apply') { options.apply = true; continue; }
    if (!known.has(key) || !argv[i + 1] || argv[i + 1].startsWith('--')) throw new Error('Unknown option or missing value; use --help');
    options[key] = argv[++i];
  }
  const project = options.project;
  if (!/^[a-z][a-z0-9-]{4,28}[a-z0-9]$/.test(project ?? '') || project !== options['gmail-oauth-project']) throw new Error('--project must be a project ID matching explicit --gmail-oauth-project');
  const endpoint = publicHttps(options['push-endpoint'], '--push-endpoint');
  const audience = publicHttps(options.audience ?? endpoint, '--audience');
  const topic = options.topic ?? 'gmail-push', subscription = options.subscription ?? 'gmail-push', sa = options['service-account'] ?? 'gmail-push';
  for (const name of [topic, subscription]) if (!/^[A-Za-z][A-Za-z0-9._~+%-]{2,254}$/.test(name) || name.toLowerCase().startsWith('goog')) throw new Error('Invalid topic or subscription name');
  if (!/^[a-z][a-z0-9-]{4,28}[a-z0-9]$/.test(sa)) throw new Error('Invalid service-account ID (6–30 characters)');
  const email = `${sa}@${project}.iam.gserviceaccount.com`, topicPath = `projects/${project}/topics/${topic}`, subscriptionPath = `projects/${project}/subscriptions/${subscription}`;
  const summary = { project, topic: topicPath, subscription: subscriptionPath, pushEndpoint: endpoint, oidcAudience: audience, oidcServiceAccountEmail: email };
  if (!options.apply) { log('Dry-run: no gcloud commands executed. Apply will enable Pub/Sub, check/reuse resources, grant Gmail publisher and Pub/Sub token minting IAM, and create an authenticated push subscription if absent.'); log(JSON.stringify(summary, null, 2)); return summary; }
  function call(args) {
    try { return run([...args, `--project=${project}`, '--quiet', '--format=json']); }
    catch { throw new Error(`gcloud failed during ${args.slice(0, 3).join(' ')}; stopped. Check gcloud authentication and permissions locally (provider diagnostics suppressed).`); }
  }
  const p = call(['projects', 'describe', project]);
  if (p.projectId !== project || p.lifecycleState !== 'ACTIVE' || !/^\d+$/.test(p.projectNumber ?? '')) throw new Error('Project response is not the expected active project');
  call(['services', 'enable', 'pubsub.googleapis.com']);
  function list(args) { const value = call(args); if (!Array.isArray(value)) throw new Error('Unexpected gcloud list response; stopped'); return value; }
  const topics = list(['pubsub', 'topics', 'list']);
  const accounts = list(['iam', 'service-accounts', 'list']);
  const subscriptions = list(['pubsub', 'subscriptions', 'list']);
  const existing = subscriptions.find(s => s.name === subscriptionPath);
  if (existing && (existing.topic !== topicPath || existing.pushConfig?.pushEndpoint !== endpoint || existing.pushConfig?.oidcToken?.serviceAccountEmail !== email || existing.pushConfig?.oidcToken?.audience !== audience || existing.pushConfig?.noWrapper)) throw new Error('Existing subscription is incompatible; use a different name or reconcile it manually');
  const account = accounts.find(a => a.email === email);
  if (account?.disabled) throw new Error('Existing push service account is disabled');
  if (!topics.some(t => t.name === topicPath)) call(['pubsub', 'topics', 'create', topic]);
  if (!account) call(['iam', 'service-accounts', 'create', sa]);
  call(['beta', 'services', 'identity', 'create', '--service=pubsub.googleapis.com']);
  call(['pubsub', 'topics', 'add-iam-policy-binding', topic, '--member=serviceAccount:gmail-api-push@system.gserviceaccount.com', '--role=roles/pubsub.publisher', '--condition=None']);
  call(['iam', 'service-accounts', 'add-iam-policy-binding', email, `--member=serviceAccount:service-${p.projectNumber}@gcp-sa-pubsub.iam.gserviceaccount.com`, '--role=roles/iam.serviceAccountTokenCreator', '--condition=None']);
  if (!existing) call(['pubsub', 'subscriptions', 'create', subscription, `--topic=${topicPath}`, `--push-endpoint=${endpoint}`, `--push-auth-service-account=${email}`, `--push-auth-token-audience=${audience}`]);
  log('Provisioning complete. Receiver configuration:'); log(JSON.stringify(summary, null, 2));
  return summary;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try { setup(process.argv.slice(2)); } catch (error) { console.error(error.message); process.exitCode = 1; }
}
