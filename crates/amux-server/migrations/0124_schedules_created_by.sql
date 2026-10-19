-- Who authored a schedule's delivered content (ai-config#210): the VERIFIED
-- worker identity of the create or last content-changing PATCH, '' for the
-- owner. NULL is a row written before this column existed; delivery then reads
-- the creation audit row instead (scheduler::schedule_worker_author).
-- A worker-authored schedule is never delivered as owner input.
ALTER TABLE schedules ADD COLUMN created_by TEXT;
