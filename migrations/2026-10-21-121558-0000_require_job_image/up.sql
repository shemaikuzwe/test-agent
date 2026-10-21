-- Your SQL goes here
ALTER TABLE `jobs` DROP COLUMN `image`;
ALTER TABLE `jobs` ADD COLUMN `image` TEXT NOT NULL;

