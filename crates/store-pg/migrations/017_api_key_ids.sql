-- An API key is identified by its key id: a name unique among the keys of
-- its scope (one universe, or the deployment) that survives a change of
-- secret. Until now a key was known only by the display prefix of its
-- secret, which a rotation replaces, so nothing could say that a new secret
-- belonged to an existing key.
--
-- Existing keys take a server-allocated id. Supplying the secret of such a
-- key under a chosen id later gives the key that id.
ALTER TABLE api_keys ADD COLUMN key_id text;
UPDATE api_keys SET key_id = 'api_key_' || replace(gen_random_uuid()::text, '-', '');
ALTER TABLE api_keys ALTER COLUMN key_id SET NOT NULL;
ALTER TABLE api_keys ADD CONSTRAINT api_keys_key_id_format
    CHECK (key_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$');
-- Deployment keys have no universe and still share one namespace. A revoked
-- key keeps its id: the row is the record of what that key was.
CREATE UNIQUE INDEX api_keys_scope_key_id_idx
    ON api_keys (universe_id, key_id) NULLS NOT DISTINCT;
