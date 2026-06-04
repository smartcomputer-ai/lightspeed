# Roadmap to Usable Agent

- [x] vfs
- [x] skills (P63 first cut)
- timers/schedules/cron
- search & web requests
- compaction
- vfs watches (inject/update on change)
- prompt management (ala claw)
- (go live)
- vms/sandboxes
- fleet


---

- remove instructions from session config, move to context
- remove skill specific logic (use context native primitives):
  - do not trigger skill if llm reads skill file (is just normal tool path)
- implement and test compaction