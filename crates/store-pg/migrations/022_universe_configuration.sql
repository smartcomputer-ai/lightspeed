-- Desired Platform configuration, independent of Runtime API-key authority.
ALTER TABLE universes ADD COLUMN display_name text CHECK (display_name IS NULL OR char_length(display_name) BETWEEN 1 AND 120);
ALTER TABLE universes ADD COLUMN platform_access jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(platform_access) = 'object');
