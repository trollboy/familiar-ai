ALTER TABLE worker_selections
ADD COLUMN risk_classes_json TEXT NOT NULL DEFAULT '[]';

ALTER TABLE worker_selections
ADD COLUMN expected_file_count INTEGER NOT NULL DEFAULT 0;
