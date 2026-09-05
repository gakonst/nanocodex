# Harbor comparison plans

The website publishes an A/B comparison only when a plan in this directory
validates against two retained jobs. It never infers a comparison from similar
task names.

Plans use schema version 1 and freeze:

- one dataset digest and the name/content digest of every task;
- attempt count, model, effort, system instructions, environment, verifier,
  timeout policy, resource policy, and tool availability;
- each arm's candidate provenance; and
- the exact Harbor job ID, website job key, and `lock.json` digest.

Each job directory must contain an `experiment-plan.json` marker whose `digest`
is the content digest of the plan. The sync requires both markers, both Harbor
locks, and both exact job IDs to agree with that plan. Each planned attempt also
binds the exact retained trial ID from each arm; the sync rejects synthetic or
unassigned attempts. The two jobs may differ only in their declared candidate
arm. Rows are paired by `(datasetDigest, taskDigest, attemptIndex)`.

Candidate evidence must itself be immutable: Nanocodex jobs need a pinned
`binary_sha256`, and Codex jobs need an exact resolved semantic `version` (not
`latest`, a distribution tag, or a range). A mutable local binary path is
deliberately ineligible for published comparison.

Both adapters retain the SHA-256 digests of the system prompt and AGENTS.md
content they verified during each completed trial. Jobs created before this
evidence was retained are deliberately ineligible; the sync never substitutes
content from a newer worktree checkout.

Store a plan as canonical one-line JSON followed by a newline. Its filename is
the lowercase SHA-256 of those exact bytes plus `.json`. The sync rejects
renamed, edited, incomplete, missing, or stale plans and jobs.

```json
{
  "schemaVersion": 1,
  "id": "example",
  "datasetDigest": "sha256:<64 hex>",
  "attemptCount": 1,
  "tasks": [
    {
      "name": "dataset/task",
      "digest": "sha256:<64 hex>",
      "attempts": [
        {
          "attemptIndex": 0,
          "harnessTrialId": "<Harbor trial UUID>",
          "codexTrialId": "<Harbor trial UUID>"
        }
      ]
    }
  ],
  "policy": {
    "model": "gpt-5.6-sol",
    "effort": "high",
    "systemInstructionsDigest": "sha256:<64 hex>",
    "environmentDigest": "sha256:<64 hex>",
    "verifierDigest": "sha256:<64 hex>",
    "timeoutPolicyDigest": "sha256:<64 hex>",
    "resourcePolicyDigest": "sha256:<64 hex>",
    "toolAvailabilityDigest": "sha256:<64 hex>"
  },
  "arms": {
    "harness": {
      "job": {
        "key": "<website job key>",
        "id": "<Harbor job UUID>",
        "lockDigest": "sha256:<64 hex>"
      },
      "candidateProvenanceDigest": "sha256:<64 hex>"
    },
    "codex": {
      "job": {
        "key": "<website job key>",
        "id": "<Harbor job UUID>",
        "lockDigest": "sha256:<64 hex>"
      },
      "candidateProvenanceDigest": "sha256:<64 hex>"
    }
  }
}
```
