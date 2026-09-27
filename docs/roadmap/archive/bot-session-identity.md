# Bot session identity in instructions

Status: implemented, 2026-09-09.

Bot instructions should identify the concrete conversation as well as its bot.
Include the bot ID, session ID, session kind, original per-key routing value,
and a thread label when it adds information. Explain that `bot_emit.sessionKey`
uses the original key and that omitting it routes a self-event to Main.

The routing key is preserved explicitly in admitted event routing records,
controller activity inputs, tracked sessions, continuation state, and federation
reply routes. It stays independent of labels and generation-suffixed session IDs.
Optional serialized fields let older records load without inventing missing keys.
The existing PostgreSQL JSON column stores the additional routing field; no
schema migration or public API shape change is needed.

Instructions follow the existing profile application lifecycle: newly created
sessions receive them, Main can adopt them when its profile is reapplied, and
existing routed sessions keep their setup until reset. Stored records that
predate key preservation can still produce a keyed session with an unknown key.

Validation covers keyed/chat and per-event routing, exact session generations,
quoted keys and labels, optional emit guidance, unknown legacy keys, activity
serialization, continuation state, and reply routing. No credentialed tests
are required. The 127 bot tests, 112 workflow unit tests, 35 server bot tests,
three workflow contract tests, and documentation checks pass. Workflow contract
regeneration produces no changes to the public integration artifacts.
