# Monster World vision

Monster World is a browser-owned physical world inhabited by durable individual
residents. The reducer owns position, hearing, collisions, inventory, weather,
and whether a proposed action commits. Luna supplies each resident's semantic
judgment; it does not replace the reducer or act as one shared town narrator.

## One resident, one session

Every resident has one distinct, long-lived Agent session keyed by `ResidentId`.
That session retains only that resident's identity and conversation history.
Sessions are created lazily, concurrent creation for the same resident is
deduplicated, and an interrupted turn does not destroy its resident's session.
Only World shutdown releases retained sessions.

Bounded concurrent turn slots are scheduling policy, not shared lanes or shared
history. An idle resident may keep an idle session. Increasing the population
must never multiplex multiple identities through one Agent.

## Situated coordination contract

Each decision receives the resident's exact scene, position, facing, intended
destination, nearby identities and relative positions, global public roster,
and the stable ordered identities of co-listeners to the same Scout utterance.
The resident interprets natural language independently from that current state.

For group spatial work, stable co-listener ordering provides distinct reference
slots. Six residents can form one deterministic ring around Scout or split into
two balanced, vertically spaced sides without a page-level phrase parser taking
over their semantic decision. Later observations let each resident correct for
terrain or crowding.

The physical reducer reserves distinct destinations, prevents overlapping
steps and head-on swaps, and reroutes a stale path around current occupants.
Those safety rules constrain proposed actions without deciding the social or
semantic meaning of Scout's request.

These ownership and coordination properties are the prototype contract. Later
latency, batching, or token optimizations must preserve resident-specific
history, per-resident cancellation, and reducer-owned physical safety.
