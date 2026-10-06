import { readFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
// Baileys rc14 expects rejection by throw, while libsignal 6 returns false.
// Preserve the maintained verifier and reject its explicit false result.
export const verifySignaturePlugin = {name:'baileys-libsignal-verification',setup(build){
  build.onLoad({filter:/@whiskeysockets\/baileys\/lib\/Utils\/crypto\.js$/},async ({path})=>{
    const source=await readFile(path,'utf8');
    // Fail closed on upstream changes before replacing the pairing-only KDF.
    if (createHash('sha256').update(source).digest('hex') !== '4d12873691b0a50e7db75b7e69df37f0f2f0ebf0e93f62fa7cbda9ff1ecc99d9') {
      throw new Error('Pinned Baileys crypto source changed; adapter review required');
    }
    const old='curve.verifySignature(generateSignalPubKey(pubKey), message, signature);';
    if(!source.includes(old))throw new Error('Baileys verification adapter requires review after upgrade');
    let patched = source.replace(old,'if (curve.verifySignature(generateSignalPubKey(pubKey), message, signature) === false) return false;');
    // workerd's node:crypto empty-AAD call fails GCM authentication. Omitting
    // zero-length AAD is the identical AEAD input and preserves native crypto.
    for (const name of ['cipher', 'decipher']) {
      const aad = `${name}.setAAD(additionalData);`;
      if (!patched.includes(aad)) throw new Error('Baileys GCM adapter requires review after upgrade');
      patched = patched.replace(aad, `if (additionalData.byteLength) ${aad}`);
    }
    const pairingStart = patched.indexOf('export async function derivePairingCodeKey(pairingCode, salt) {');
    const pairingEnd = patched.indexOf('\n}', pairingStart) + 2;
    if (pairingStart < 0 || pairingEnd <= pairingStart) throw new Error('Baileys pairing KDF adapter requires review after upgrade');
    const adapter = fileURLToPath(new URL('../../src/whatsapp-adapters/pairing-code-key.js', import.meta.url));
    patched = patched.slice(0, pairingStart) + `export { derivePairingCodeKey } from ${JSON.stringify(adapter)};` + patched.slice(pairingEnd);
    return {contents:patched,loader:'js'};
  });
}};
