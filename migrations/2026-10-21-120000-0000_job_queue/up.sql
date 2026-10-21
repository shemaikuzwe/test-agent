-- Queue ordering and lookup of pending work.
ALTER TABLE `jobs` ADD COLUMN `created_at` BIGINT NOT NULL DEFAULT 0;
UPDATE `jobs` SET `created_at` = `updated_at`;
CREATE INDEX `jobs_state_created_at` ON `jobs`(`state`, `created_at`);
