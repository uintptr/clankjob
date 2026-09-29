-- Approvals (design §9.7) and plugin wait-condition checks (design §6.2).

-- A human request is a question or an approval of a tool call.
ALTER TABLE human_requests ADD COLUMN kind TEXT NOT NULL DEFAULT 'question';
ALTER TABLE human_requests ADD COLUMN tool TEXT;
-- JSON arguments; replaced by the owner's edit when approved.
ALTER TABLE human_requests ADD COLUMN args TEXT;
-- approve / reject
ALTER TABLE human_requests ADD COLUMN decision TEXT;
-- For approved calls: pending, then running (committed before the tool runs), then done.
ALTER TABLE human_requests ADD COLUMN execution TEXT;
CREATE INDEX human_requests_execution ON human_requests (case_id, execution);

-- Where a plugin condition's check continues, how often it runs, and how many checks
-- in a row failed.
ALTER TABLE wait_conditions ADD COLUMN cursor TEXT;
ALTER TABLE wait_conditions ADD COLUMN check_every_ms INTEGER;
ALTER TABLE wait_conditions ADD COLUMN failures INTEGER NOT NULL DEFAULT 0;
