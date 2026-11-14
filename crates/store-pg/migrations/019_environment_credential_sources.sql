-- What may be bound into an environment.
--
-- A bound credential is handed to the commands of the machine in plain
-- form, so a binding is a way to read the value. Two sources remain: a
-- credential grant, which must be retrievable, and the stored API key of a
-- model API key provider. A bare secret id is no longer a source: it named
-- any stored secret of the universe (an OAuth refresh token, a GitHub App
-- private key) with no exposure rule to check.

-- Bindings that named a secret directly cannot be expressed any more.
DELETE FROM environment_credentials WHERE source_kind = 'direct_secret';

ALTER TABLE environment_credentials
    DROP CONSTRAINT environment_credentials_source_kind_known,
    DROP CONSTRAINT environment_credentials_source_exactly_one;

-- Dropping the column also drops its foreign key into auth_secrets.
ALTER TABLE environment_credentials DROP COLUMN secret_id;

ALTER TABLE environment_credentials
    ADD CONSTRAINT environment_credentials_source_kind_known
        CHECK (source_kind IN ('auth_grant', 'auth_provider_credential')),
    ADD CONSTRAINT environment_credentials_source_exactly_one CHECK (
        (source_kind = 'auth_grant' AND grant_id IS NOT NULL AND auth_provider_id IS NULL)
        OR (source_kind = 'auth_provider_credential' AND grant_id IS NULL AND auth_provider_id IS NOT NULL)
    );

-- A grant that is already bound into an environment already has its plain
-- value handed to that machine, so recording it as retrievable states what
-- is true and keeps the binding working. Exposure is otherwise fixed at
-- creation; nothing in the schema enforces that, the service does.
UPDATE auth_grants g
SET exposure = 'retrievable'
WHERE g.exposure = 'brokered'
  AND EXISTS (
      SELECT 1
      FROM environment_credentials c
      WHERE c.universe_id = g.universe_id
        AND c.grant_id = g.grant_id
  );

COMMENT ON TABLE environment_credentials IS
    'Universe-owned credential bindings for an environment: a retrievable grant or the API key of a model API key provider.';
COMMENT ON COLUMN auth_grants.exposure IS
    'Creation-time authority, immutable through the service: brokered grants stay inside the token broker; only retrievable grants may be leased to trusted services or injected into environments.';
