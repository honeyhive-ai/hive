# Collaborative editing

When multiple peers share a workspace, edits to the same state need a merge
strategy. Hive's implemented model is **event-level last-write-wins**, with a
character-level CRDT for concurrent free-text editing tracked as a follow-up.

## Event-level convergence (implemented)

Every state mutation is a `SessionEvent` wrapped in a signed
`SessionEventEnvelope`. The append-only envelope log is the source of truth, and
the projector folds events deterministically — so replicas that have seen the
same set of envelopes converge to the same state regardless of arrival order.
Events are deduped by `eventId`, so restarts and double-sends are harmless.

For most fields this is exactly right: titles, role assignments, runtime
selection, reactions, proposal create/vote, roster changes. Collisions on these
are rare or low-stakes, and the deterministic event order resolves them.

The chat transcript is **append-only** at the envelope level — concurrent
messages from different peers all land; there's no in-place edit to conflict
over.

## Concurrent free-text editing (roadmap)

True character-level concurrent editing of a single text field (e.g. two people
typing into the same proposal body at once, Google-Docs style) needs a CRDT
layer on top of the envelope log — a Replicated Growable Array for text plus a
cursor/presence overlay. That is a **tracked follow-up and not wired in the
current build**. Until it lands, proposal bodies use the event-level model
above.
