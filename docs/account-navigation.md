# Account navigation links

Call `GET /v1/account/links` from the account API or `https://api.nanocodex.xyz` to get browser URLs for Connections, Vault, Wallet, and API access. These are ordinary navigation links: visitors sign into their own account, and no account credential or grant is included in the URL.

```sh
curl 'https://api.nanocodex.xyz/v1/account/links?connect=github&add=login'
```

The cloud SDK exposes the same operation before or after connecting:

```js
import { Client } from 'nanocodex/cloud';

const client = Client.create({ appId: 'example-app' });
const links = await client.account.links({ connect: 'github', add: 'login' });
// Render links.connections or links.vault as a normal link in your app.
```

`connections`, `vault`, `wallet`, and `access` point to `/connect`, `/connect/vault`, `/connect/wallet`, and `/connect/access`. A Vault add link preserves the selected form through sign-in. A connector link focuses the selected provider; connecting still requires an explicit action.

Optional `connect` accepts `cloudflare`, `github`, `google`, `slack`, `x`, `spotify`, `soundcloud`, `link`, `whatsapp`, `claude`, `chatgpt`, `openai`, or `mcp`. Optional `add` accepts `login`, `api_key`, `card`, `address`, `phone`, or `totp`. Unknown, empty, or repeated parameters return 400; non-GET requests return 405. Responses use `Cache-Control: no-store`.

Production URLs open the account web app. Loopback and Nanocodex local development retain their origin. Neither request headers nor query parameters can supply an alternative destination.
