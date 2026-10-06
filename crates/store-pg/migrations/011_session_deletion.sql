-- Soft deletion is a storage lifecycle, separate from the event log.
ALTER TABLE sessions ADD COLUMN deleted_at_ms bigint;
ALTER TABLE sessions ADD CONSTRAINT sessions_deleted_closed CHECK
    (deleted_at_ms IS NULL OR lifecycle_status = 'closed');
