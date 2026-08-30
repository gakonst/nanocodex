# Nanocodex launch video storyboards

The videos should show what a product developer or user gets before explaining
the infrastructure. Each clip begins inside an embedded product, contains one
visible transition, and ends with a useful result. Avoid terminal montages and
conceptual animations when the real product flow can be recorded.

Produce silent, captioned MP4 and WebM variants at 1440x810, plus a 1080x1350
crop for social. Keep pointer movement deliberate, hide personal data, and use
synthetic repositories and accounts. Export a reduced-motion version that
replaces zooms and cursor travel with cuts. The blog should set a poster image,
`playsinline`, `muted`, `loop`, and `controls`; autoplay is optional and must
respect reduced-motion preferences.

## 1. Embed one agent across products

Duration: 24 seconds.

1. Inside a web product, enter: “Audit this repository and draft the migration
   PR.”
2. Show the turn accepted and the first durable output events rendered in the
   product.
3. Detach the web client while the hosted turn remains `running`.
4. Open the same agent in Slack or a phone-sized client and resume after the
   last processed event.
5. End on the proposed change and completed status without replaying the
   prompt or transcript.

On-screen copy: “One agent. Any product. The work survives the interface.”

This is the first clip in the post. It should make Embed and the managed-agent
contract legible without mentioning journals, process boundaries, or Centaur.

## 2. Connect once, grant anywhere

Duration: 30 seconds.

1. In the first embedded product, click “Use my ChatGPT subscription.”
2. Complete OpenAI's device authorization and return to the product.
3. Connect GitHub and approve the requested product grant.
4. Ask the agent to open a pull request in a synthetic repository.
5. Open a second Nanocodex-powered product with the same account. Show ChatGPT
   and GitHub already present, then approve a different bounded grant.
6. Add Slack from the second product and show it become available in the first
   account view without exposing a token to either product.

On-screen copy: “Connect once. Grant each product only what it needs.”

Never display a real token, even blurred. Capture the product receiving
capability identities and the successful brokered action. Inspect the execution
environment separately to verify that no reusable credential entered it.

## 3. Keep the brain, attach the hands

Duration: 20 seconds.

1. Ask the agent to inspect and edit a small text project.
2. Show the work beginning immediately in the lightweight Rust/WASM workspace.
3. Change the request to run a native package or full browser test.
4. Show the agent request a typed capability and policy approve an isolated
   environment.
5. Show the same turn continue against the attached machine, then release that
   machine after the result is committed.

On-screen copy: “Pay for the machine when the work needs it.”

The activity UI should distinguish the long-lived managed agent from the
short-lived execution hand. If the product pre-attaches a sandbox, record that
as a separate integration example rather than weakening this cost proof.

## 4. Leave with the runnable state

Duration: 28 seconds.

1. Open a hosted agent with completed work and a known event cursor.
2. Request a consistent portable snapshot from the Paradigm API.
3. Import it into a clean Postgres-backed self-hosted deployment, or one of the
   checked-in Cloudflare or Vercel adapters.
4. Show the source writer fenced before activating the destination.
5. Reauthorize the connector at the destination.
6. Ask a follow-up question that depends on earlier committed history and show
   the resumed result.

On-screen copy: “Export runnable state, not only a transcript.”

Do not replace this with an architecture animation. The portability claim is
credible only when export, fencing, import, reauthorization, and resume are
recorded end to end.

## Capture checklist

- Use the production behavior matrix and one release commit for every clip.
- Record network and console logs separately for review, but keep them out of
  the final edit unless they clarify a transition.
- Verify every displayed cursor and status is produced by the API, not overlaid
  in post-production.
- Include captions in the media file and a text transcript beside the blog
  embed.
- Add a poster image that communicates the outcome with video disabled.
- Re-record if launch policy differs from the storyboard; do not let the video
  promise a capability the hosted endpoint does not implement.
