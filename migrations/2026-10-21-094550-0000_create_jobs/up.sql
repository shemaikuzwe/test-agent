-- Your SQL goes here
CREATE TABLE `jobs`(
	`deployment_id` TEXT NOT NULL PRIMARY KEY,
	`node_id` TEXT NOT NULL,
	`state` TEXT NOT NULL,
	`image` TEXT,
	`error` TEXT,
	`updated_at` BIGINT NOT NULL,
	`pending_status` BOOL NOT NULL
);

