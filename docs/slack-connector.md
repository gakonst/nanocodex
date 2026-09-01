# Slack connector

Nanocodex connects Slack as the signed-in Slack user, not as a bot. Each
workspace installation is retained as a separate encrypted connector owned by
the Nanocodex account. Connecting a second workspace does not replace the
first.

## Slack application

Create a Slack OAuth application and configure the production redirect URL:

```text
https://nanocodex.gakonst.workers.dev/v1/connectors/slack/callback
```

Local development uses the shared fixed relay callback. Configure this exact
URL in the same Slack application before testing the local flow:

```text
http://127.0.0.1:47891/v1/connectors/slack/callback
```

The connector requests user-token scopes for channel, private-channel, DM and
group-DM history; conversation discovery; message and reaction writes; search;
and user lookup. It deliberately requests `user_scope` and retains
`authed_user.access_token`, so `chat.postMessage` and other mutations execute as
the connected person. It does not request Slack admin scopes.

Set the OAuth application credentials as a pair:

```text
NANOCODEX_SLACK_OAUTH_CLIENT_ID=...
NANOCODEX_SLACK_OAUTH_CLIENT_SECRET=...
```

## Multiple workspaces

The public connector status includes one non-secret entry per workspace:

```json
{
  "slack": {
    "connected": true,
    "connections": [
      {
        "id": "T01234567",
        "workspace": "Acme",
        "user_id": "U01234567",
        "label": "Acme (U01234567)"
      }
    ]
  }
}
```

The agent selects a workspace on Slack API requests with the internal
`x-nanocodex-connector-instance` header. The value is the workspace connection
ID. The broker removes that header, injects the matching user token, bounds the
request and response, and never projects the token back to the agent.

Connect grants store Slack workspaces as `slack:<workspace-id>` capabilities.
A generic Slack approval is expanded to the exact connected workspaces when the
grant is issued. Workspaces connected later are therefore unavailable to that
existing grant until the user approves a new grant.
