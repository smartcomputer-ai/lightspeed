-- Defaults are universe policy; provider transport and credentials stay in
-- auth_providers. Retain a row after clears to preserve its revision.
CREATE TABLE universe_model_defaults (
    universe_id uuid PRIMARY KEY REFERENCES universes (universe_id) ON DELETE CASCADE,
    revision bigint NOT NULL CHECK (revision > 0),
    agent_run jsonb,
    speech_to_text jsonb
);
