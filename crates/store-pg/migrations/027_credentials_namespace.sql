-- Outbound provider credentials have their own namespace, separate from
-- inbound runtime authentication. Renames preserve encrypted values, IDs,
-- timestamps, and every existing relationship without copying token data.
ALTER TABLE auth_clients RENAME TO credentials_clients;
ALTER TABLE auth_flows RENAME TO credentials_flows;
ALTER TABLE auth_grants RENAME TO credentials_grants;
ALTER TABLE auth_provider_headers RENAME TO credentials_provider_headers;
ALTER TABLE auth_providers RENAME TO credentials_providers;
ALTER TABLE auth_secrets RENAME TO credentials_secrets;

-- PostgreSQL follows foreign keys when their referenced relation is renamed.
-- Give the renamed relations' own constraints and indexes matching names too.
-- Contextual columns and constraints on other tables keep their existing names.
DO $$
DECLARE
    entry record;
BEGIN
    FOR entry IN
        SELECT relation.relname AS table_name, constraint_record.conname AS constraint_name
        FROM pg_constraint AS constraint_record
        JOIN pg_class AS relation ON relation.oid = constraint_record.conrelid
        JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
        WHERE namespace.nspname = current_schema()
          AND relation.relname IN (
              'credentials_clients', 'credentials_flows', 'credentials_grants',
              'credentials_provider_headers', 'credentials_providers', 'credentials_secrets'
          )
          AND starts_with(constraint_record.conname, 'auth_')
    LOOP
        EXECUTE format('ALTER TABLE %I.%I RENAME CONSTRAINT %I TO %I',
            current_schema(), entry.table_name, entry.constraint_name,
            'credentials_' || substr(entry.constraint_name, 6));
    END LOOP;

    -- Renaming primary/unique constraints also renames their backing indexes.
    -- This loop handles the remaining non-constraint indexes.
    FOR entry IN
        SELECT indexname
        FROM pg_indexes
        WHERE schemaname = current_schema()
          AND tablename IN (
              'credentials_clients', 'credentials_flows', 'credentials_grants',
              'credentials_provider_headers', 'credentials_providers', 'credentials_secrets'
          )
          AND starts_with(indexname, 'auth_')
    LOOP
        EXECUTE format('ALTER INDEX %I.%I RENAME TO %I',
            current_schema(), entry.indexname,
            'credentials_' || substr(entry.indexname, 6));
    END LOOP;
END
$$;

COMMENT ON COLUMN credentials_provider_headers.grant_id IS
    'Universe credential grant whose token is resolved per request and sent raw; the token never enters this table.';
