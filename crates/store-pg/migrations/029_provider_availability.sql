-- A provider is available to every universe, or only to universes with a
-- binding. A binding no longer has an on/off state: its row is the
-- allowance. Existing providers keep their per-universe allowances.
ALTER TABLE environment_providers
    ADD COLUMN available_to_all_universes boolean NOT NULL DEFAULT false;
DELETE FROM environment_provider_bindings WHERE NOT enabled;
ALTER TABLE environment_provider_bindings DROP COLUMN enabled;
COMMENT ON COLUMN environment_providers.available_to_all_universes IS
    'Every universe may use the provider; otherwise only universes with a binding.';
COMMENT ON TABLE environment_provider_bindings IS
    'Universes allowed to use a provider that is not available to all universes.';
