-- This file should undo anything in `up.sql`
ALTER TABLE `jobs` DROP COLUMN `port`;
ALTER TABLE `jobs` DROP COLUMN `hostname`;

