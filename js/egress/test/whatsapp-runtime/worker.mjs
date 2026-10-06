import { restoreAuthBuffers } from '../../src/whatsapp-adapters/auth-buffers.ts';
import { inflate, deflateSync } from 'node:zlib';
import { promisify } from 'node:util';
import makeWASocket, { Browsers, DEFAULT_CONNECTION_CONFIG, generateSignalPubKey, Curve, aesEncryptGCM, aesDecryptGCM, aesEncryptCTR, aesDecryptCTR, aesEncrypt, aesDecrypt, hkdf, md5, initAuthCreds, proto, derivePairingCodeKey } from '../../src/whatsapp-generated/baileys.js';
import WorkersWebSocket from '../../src/whatsapp-adapters/workers-ws.js';
import { Buffer } from 'node:buffer';
const logger = { level:'silent', child(){return this}, trace(){},debug(){},info(){},warn(){},error(){},fatal(){} };
function assert(value, label) { if (!value) throw new Error(label); }
// The real Signal lifecycle needs authenticated traffic, so exercise its maintained
// repository with two synthetic peers in workerd. Only auth storage is in memory;
// session establishment, encryption, decryption and rollover use the shipped bundle.
async function verifySignalRolloverPrivacy() {
  const makeAuth = () => {
    const data = new Map();
    return { creds: initAuthCreds(), keys: {
      async get(type, ids) {
        return Object.fromEntries(ids.filter(id => data.has(type + ':' + id)).map(id => [id, data.get(type + ':' + id)]));
      },
      async set(updates) {
        for (const [type, entries] of Object.entries(updates)) for (const [id, value] of Object.entries(entries)) {
          if (value === null) data.delete(type + ':' + id);
          else data.set(type + ':' + id, value);
        }
      },
      async transaction(work) { return await work(); },
    } };
  };
  const aliceAuth = makeAuth(), bobAuth = makeAuth();
  const alice = DEFAULT_CONNECTION_CONFIG.makeSignalRepository(aliceAuth, logger);
  const bob = DEFAULT_CONNECTION_CONFIG.makeSignalRepository(bobAuth, logger);
  const aliceJid = '15550000001@s.whatsapp.net', bobJid = '15550000002@s.whatsapp.net';
  const methods = ['log', 'info', 'warn', 'error', 'debug', 'trace', 'dir', 'table'];
  const originals = methods.map(method => console[method]);
  let consoleCalls = 0;
  // Never retain or print logged arguments, even on regression failure.
  for (const method of methods) console[method] = () => { consoleCalls++; };
  try {
    let previousAliceKey, previousBobKey;
    for (let round = 0; round < 2; round++) {
      await alice.injectE2ESession({ jid: bobJid, session: {
        registrationId: bobAuth.creds.registrationId,
        identityKey: generateSignalPubKey(bobAuth.creds.signedIdentityKey.public),
        signedPreKey: {
          keyId: bobAuth.creds.signedPreKey.keyId,
          publicKey: generateSignalPubKey(bobAuth.creds.signedPreKey.keyPair.public),
          signature: bobAuth.creds.signedPreKey.signature,
        },
      } });
      const plaintext = Buffer.from('synthetic Signal rollover ' + round);
      const encrypted = await alice.encryptMessage({ jid: bobJid, data: plaintext });
      assert(encrypted.type === 'pkmsg', 'Signal prekey session establishment');
      const decrypted = await bob.decryptMessage({ jid: aliceJid, ...encrypted });
      assert(Buffer.from(decrypted).equals(plaintext), 'Signal rollover message roundtrip');
      const aliceInfo = await alice.getSessionInfo(bobJid), bobInfo = await bob.getSessionInfo(aliceJid);
      assert(aliceInfo && bobInfo, 'Signal peer sessions established');
      if (round) {
        assert(!Buffer.from(aliceInfo.baseKey).equals(previousAliceKey), 'Signal outgoing session replaced');
        assert(!Buffer.from(bobInfo.baseKey).equals(previousBobKey), 'Signal incoming session replaced');
      }
      previousAliceKey = Buffer.from(aliceInfo.baseKey);
      previousBobKey = Buffer.from(bobInfo.baseKey);
    }
  } finally {
    try { alice.close(); bob.close(); }
    finally { methods.forEach((method, index) => { console[method] = originals[index]; }); }
  }
  assert(consoleCalls === 0, 'Signal rollover must emit no console calls; observed ' + consoleCalls);
}

export default {
  async fetch(request, env) {
    const checks = [];
    const diagnostics = [];
    try {
      assert(typeof makeWASocket === 'function', 'Baileys real import'); checks.push('baileys-import');
      const alice = Curve.generateKeyPair(), bob = Curve.generateKeyPair();
      assert(Curve.sharedKey(alice.private,bob.public).equals(Curve.sharedKey(bob.private,alice.public)), 'ECDH'); checks.push('libsignal-x25519');
      const msg = Buffer.from('workerd-protocol-test'), signature = Curve.sign(alice.private,msg);
      assert(Curve.verify(alice.public,msg,signature), 'signature');
      assert(!Curve.verify(alice.public,Buffer.from('tampered'),signature), 'reject tamper'); checks.push('libsignal-signature');
      const key = Buffer.alloc(32,7), iv = Buffer.alloc(12,1), aad = Buffer.from('aad');
      const encrypted = aesEncryptGCM(msg,key,iv,aad);
      assert(aesDecryptGCM(encrypted,key,iv,aad).equals(msg),'AES-GCM');
      encrypted[0] ^= 1; let rejected = false;
      try { aesDecryptGCM(encrypted,key,iv,aad); } catch { rejected = true; }
      assert(rejected,'AES-GCM authentication'); checks.push('aes-gcm-authenticated');
      assert(aesDecryptCTR(aesEncryptCTR(msg,key,Buffer.alloc(16)),key,Buffer.alloc(16)).equals(msg),'CTR'); checks.push('aes-ctr');
      assert(aesDecrypt(aesEncrypt(msg,key),key).equals(msg),'CBC'); checks.push('aes-cbc');
      assert(Buffer.from(md5(Buffer.from('abc'))).toString('hex') === '900150983cd24fb0d6963f7d28e17f72','WASM md5'); checks.push('static-wasm-md5');
      const out = await hkdf(Buffer.alloc(22,0x0b),42,{salt:Buffer.alloc(0),info:''});
      // RFC 5869 test case 3, empty salt and info.
      assert(Buffer.from(out).toString('hex') === '8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8','WASM HKDF RFC 5869'); checks.push('static-wasm-hkdf');
      const inflated = await Promise.race([promisify(inflate)(deflateSync(msg)),new Promise((_,reject)=>setTimeout(()=>reject(new Error('Zlib callback timeout')),3000))]);
      assert(Buffer.from(inflated).equals(msg),'zlib roundtrip'); checks.push('zlib-inflate-callback');
      assert(aesDecryptGCM(aesEncryptGCM(msg,key,new Uint8Array(12),Buffer.alloc(0)),key,new Uint8Array(12),Buffer.alloc(0)).equals(msg),'GCM empty AAD'); checks.push('aes-gcm-empty-aad');
      const emptyAadCiphertext=aesEncryptGCM(msg,key,new Uint8Array(12),Buffer.alloc(0)); emptyAadCiphertext[0]^=1; let emptyAadRejected=false;
      try {aesDecryptGCM(emptyAadCiphertext,key,new Uint8Array(12),Buffer.alloc(0));} catch {emptyAadRejected=true;}
      assert(emptyAadRejected,'empty-AAD tamper rejection'); checks.push('aes-gcm-empty-aad-tamper-rejected');
      // Full expected keys computed independently with Node's native pbkdf2Sync
      // (SHA-256, 131072 iterations, 32 bytes). Inputs are public synthetic data.
      // Exercise the exported shipped bundle, including the guarded source adapter.
      const pairingVectors = [
        { label: 'ascii', code: 'SYNTHETIC', salt: Uint8Array.from({ length: 32 }, (_, i) => i),
          expected: '9b1d1bb1f361c01c7f8ae5b9c8ce73b039d646a1b2c2a10ad3bfc8e92bd8ecf7' },
        { label: 'utf8-offset-salt', code: 'tést🔑', salt: Uint8Array.from({ length: 34 }, (_, i) => 255 - i).subarray(1, 33),
          expected: '5e2e9fdd0dfcbd1444d1756ec728292bbf40edd5bad66c4e39520aba8843c80d' },
      ];
      for (const vector of pairingVectors) {
        const derived = await derivePairingCodeKey(vector.code, vector.salt);
        assert(Buffer.isBuffer(derived) && derived.toString('hex') === vector.expected, 'pairing PBKDF2 exact key: ' + vector.label);
        checks.push('pairing-pbkdf2-131072-' + vector.label);
      }
      const creds=initAuthCreds(); assert(creds.noiseKey.private.length===32 && !creds.registered,'creds'); checks.push('auth-creds-in-memory-only');
      const encoded=proto.Message.encode({conversation:'local-test'}).finish();
      assert(proto.Message.decode(encoded).conversation==='local-test','protobuf'); checks.push('protobuf-roundtrip');
      await verifySignalRolloverPrivacy(); checks.push('signal-session-rollover-roundtrip-no-console');
      if (new URL(request.url).pathname === '/upstream') {
        const labels = new Set(['connected to WA', 'handshake recv from WA', 'not logged in, attempting registration...', 'Noise handler transitioned to Transport state', 'error in validating connection', 'connection errored', 'connection closed']);
        const record = (...args) => {
          if (diagnostics.length >= 80) return;
          const label = args.find(arg => typeof arg === 'string' && labels.has(arg));
          if (label) diagnostics.push({ label });
        };
        const safeLogger = { level: 'silent', child(){return this},trace:record,debug:record,info:record,warn:record,error:record,fatal:record };
        const restoredCreds=restoreAuthBuffers(structuredClone(initAuthCreds()));
        assert(Buffer.isBuffer(restoredCreds.noiseKey.private),'restored auth Buffer'); checks.push('persisted-auth-buffer-revival');
        const sock=makeWASocket({logger:safeLogger,auth:{creds:restoredCreds,keys:{get:async()=>({}),set:async()=>{}}},markOnlineOnConnect:false,browser:Browsers.ubuntu('Desktop'),syncFullHistory:true,connectTimeoutMs:25000});
        sock.ws.on('CB:iq,type:set,pair-device', () => diagnostics.push({label:'pair-device-received'}));
        sock.ws.on('frame', frame => {if(diagnostics.length<80)diagnostics.push({label:'decoded-frame',kind:frame instanceof Uint8Array?'binary':'node'});});
        sock.ws.on('message', data => {if(diagnostics.length<80)diagnostics.push({label:'frame-received',length:data.length});});
        sock.ws.on('close', code => {if(diagnostics.length<80)diagnostics.push({label:'socket-close',code:typeof code==='number'?code:null});});
        try {
          const handshake=new Promise((resolve,reject)=>{
            const timer=setTimeout(()=>reject(new Error('Anonymous handshake timeout')),30000);
            sock.ev.on('connection.update',update=>{
              if(update.qr){clearTimeout(timer);resolve(true);}
              if(update.connection==='close'){clearTimeout(timer);diagnostics.push({label:'connection-close',status:update.lastDisconnect?.error?.output?.statusCode??null});reject(new Error('Anonymous upstream closed'));}
            });
          });
          await sock.waitForSocketOpen(); checks.push('anonymous-upstream-websocket-open');
          await handshake; checks.push('anonymous-noise-handshake-pairing-ready');
        } finally {sock.end(undefined);}
        return Response.json({ok:true,checks,diagnostics});
      }
      const ws = new WorkersWebSocket(env.ECHO_URL,{handshakeTimeout:2000,headers:{'X-Test':'adapter'}});
      await new Promise((resolve,reject)=>{ws.once('open',resolve);ws.once('error',reject)});
      checks.push('websocket-open'); let called=false;
      const reply = new Promise((resolve,reject)=>{ws.once('message',resolve);ws.once('error',reject)});
      ws.send(Buffer.from([0,1,2,255]), error=>{if(error)throw error;called=true});
      const received = await reply;
      assert(Buffer.isBuffer(received)&&received.equals(Buffer.from([0,1,2,255])),'binary WS echo'); assert(called,'send callback');
      const closed = new Promise(resolve=>ws.once('close',resolve)); ws.close(); await closed; assert(ws.readyState===WorkersWebSocket.CLOSED,'closed'); checks.push('actual-workerd-outbound-websocket-binary-callback-close');
      return Response.json({ok:true,checks});
    } catch(error) {return Response.json({ok:false,checks,diagnostics,error:String(error)},{status:500});}
  }
};
