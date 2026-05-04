-- Add date_queued to track when a paste was last enqueued for deletion.
-- NULL means it has never been queued.  The sweeper uses this to avoid
-- re-queuing the same paste within a one-hour window.
ALTER TABLE pastes
    ADD COLUMN date_queued TIMESTAMPTZ;

