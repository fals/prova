-- Replication to toe serve (ADR 0014 section 3).
-- synced_seq: on a client, the highest event seq the server acknowledged.
-- synced_ms:  on the server, when the client last synced this run.
ALTER TABLE runs ADD COLUMN synced_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE runs ADD COLUMN synced_ms INTEGER;
