# Nanocodex launch video storyboards

The blog videos should prove product consequences before explaining the
architecture. Each clip begins with a person doing something in a product,
contains one visible system transition, and ends with a useful result. Avoid a
terminal-only montage: the audience should understand why the boundary matters
without reading implementation details.

Produce silent, captioned MP4 and WebM variants at 1440x810, plus a 1080x1350
crop for social. Keep pointer movement deliberate, hide personal data, and use
synthetic repositories and accounts. Export a reduced-motion version that
replaces zooms and cursor travel with cuts. The blog should set a poster image,
`playsinline`, `muted`, `loop`, and `controls`; autoplay is optional and must
respect reduced-motion preferences.

## 1. The agent outlives the client

Duration: 24 seconds.

1. In a product UI, enter: “Audit this repository and draft the migration PR.”
2. Show `turn accepted` and the first two durable event cursors.
3. Click a visible **Detach output** control and show the UI stop consuming
   events while the hosted turn remains `running`.
4. Close the laptop. Open the same agent from a phone-sized client, reconnect after the last
   processed cursor, and show later events without transcript replay.
5. End on the proposed change and a completed status.

On-screen copy: “The client disconnected. The work did not.”

This is the first clip in the post, directly after the attach/detach example.
It makes the product contract legible before the article introduces the
journal or the Centaur architecture.

## 2. Start cheap, attach a machine when needed

Duration: 20 seconds.

1. Ask the agent to inspect and edit a small text project.
2. Show the work beginning immediately in the Just Bash workspace.
3. Change the request to run a native package or full browser test.
4. Show a typed capability requirement in the product's activity UI.
5. Show policy approving an isolated environment and the same turn continuing
   against it.

On-screen copy: “A sandbox is a tool, not the agent's home.”

If a product explicitly pre-attaches a sandbox instead, record that path in a
separate integration clip; this launch video should demonstrate the automatic
capability transition.

## 3. Bring ChatGPT and connected tools

Duration: 30 seconds.

1. In an integrating product, click “Use my ChatGPT subscription.”
2. Complete OpenAI's device authorization and return to the product.
3. Add GitHub through the same Connect surface and show its requested grant.
4. Return to the product with `chatgpt` model access and a GitHub capability
   identity, not credentials.
5. Ask the agent to open a pull request in a synthetic repository.
6. Show the pull request result, then briefly reveal the execution environment's
   secret panel or environment inspection with no GitHub token present.

On-screen copy: “Bring your ChatGPT subscription. Keep credentials outside the agent.”

Never put a real token into the recording, even blurred. The proof is the
absence of credential material and the successful brokered action.

## 4. A credible exit

Duration: 28 seconds.

1. Open a hosted agent with a completed turn and known event cursor.
2. Request a consistent portable snapshot from the Paradigm API.
3. Import it into a clean Postgres-backed self-hosted deployment, or one of the
   checked-in Cloudflare or Vercel adapters.
4. Show the source writer fenced before activating the destination.
5. Reauthorize the connector at the destination.
6. Ask a follow-up question that depends on the earlier committed history and
   show the resumed result.

On-screen copy: “Export runnable state, not only a transcript.”

Do not replace this with an architecture animation. The portability claim is
credible only when the actual hosted export, fencing, import, and resume path
can be recorded end to end.

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
