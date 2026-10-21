DROP INDEX IF EXISTS `jobs_state_created_at`;
ALTER TABLE `jobs` DROP COLUMN `created_at`;
