-- Your SQL goes here
CREATE TABLE `jobs`(
	`deployment_id` TEXT NOT NULL PRIMARY KEY,
	`state` TEXT NOT NULL,
	`error` TEXT,
	`updated_at` BIGINT NOT NULL,
	`created_at` BIGINT NOT NULL DEFAULT 0,
	`image` TEXT NOT NULL,
	`application_id` TEXT NOT NULL,
	`port` INTEGER,
	`hostname` TEXT,
	`env` TEXT
);

CREATE INDEX `jobs_state_created_at` ON `jobs`(`state`, `created_at`);
