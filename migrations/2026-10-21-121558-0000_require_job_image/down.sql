-- This file should undo anything in `up.sql`
ALTER TABLE `jobs` DROP COLUMN `image`;
ALTER TABLE `jobs` ADD COLUMN `image` TEXT;

