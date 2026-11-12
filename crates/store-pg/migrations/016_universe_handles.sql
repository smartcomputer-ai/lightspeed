-- A universe's handle is its deployment-unique, renameable name: the readable
-- address that stays the same where the UUID differs between deployments.
-- Every universe has one. The format is a lowercase DNS label, so a handle is
-- safe unescaped in a URL path, a host name and a Kubernetes resource name,
-- and it never has the shape of a UUID, so a reference is unambiguously one
-- or the other.
ALTER TABLE universes DROP CONSTRAINT universes_slug_format;
ALTER TABLE universes RENAME COLUMN slug TO handle;
ALTER TABLE universes RENAME CONSTRAINT universes_slug_key TO universes_handle_key;

-- Existing names that already fit are kept. An unnamed universe, or one whose
-- name the format no longer admits, takes the name derived from its UUID that
-- creation uses when the caller supplies none.
UPDATE universes
SET handle = 'u-' || right(replace(universe_id::text, '-', ''), 12)
WHERE handle IS NULL
   OR handle !~ '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'
   OR handle ~ '^[0-9a-f]{32}$'
   OR handle ~ '^[0-9a-f]{8}-([0-9a-f]{4}-){3}[0-9a-f]{12}$';

ALTER TABLE universes ALTER COLUMN handle SET NOT NULL;
ALTER TABLE universes ADD CONSTRAINT universes_handle_format CHECK (
    handle ~ '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'
    AND handle !~ '^[0-9a-f]{32}$'
    AND handle !~ '^[0-9a-f]{8}-([0-9a-f]{4}-){3}[0-9a-f]{12}$'
);
