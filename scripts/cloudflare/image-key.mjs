import {fingerprint} from './managed-images.mjs';
console.log(fingerprint(process.argv[2],process.env.CLOUDFLARE_ACCOUNT_ID,process.env.MANAGED_IMAGE_CACHE_EPOCH || '1'));
