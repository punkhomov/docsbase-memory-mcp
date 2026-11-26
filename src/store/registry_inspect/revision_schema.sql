-- Generation metadata schema 2. A live update sets the durable intent before
-- opening the Tantivy writer. The final transaction records the manifest and
-- clears the intent. NULL manifest selects the original sealed file inventory.
ALTER TABLE generation_state ADD COLUMN mutation_pending INTEGER NOT NULL DEFAULT 0
    CHECK (mutation_pending IN (0, 1));
ALTER TABLE generation_state ADD COLUMN tantivy_manifest BLOB
    CHECK (tantivy_manifest IS NULL OR length(tantivy_manifest) = 32);
