-- This file should undo anything in `up.sql`
ALTER TABLE `jobs` ADD COLUMN `pending_status` BOOL NOT NULL;

