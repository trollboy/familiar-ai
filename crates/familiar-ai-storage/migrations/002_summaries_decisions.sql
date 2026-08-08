ALTER TABLE decisions ADD COLUMN confidence TEXT;

ALTER TABLE file_summaries ADD COLUMN extracted_symbols_json TEXT DEFAULT '[]';
ALTER TABLE file_summaries ADD COLUMN last_known_mtime INTEGER;
ALTER TABLE file_summaries ADD COLUMN last_known_size INTEGER;
