CREATE TABLE IF NOT EXISTS c (
	`id` integer NOT NULL PRIMARY KEY AUTOINCREMENT,
	`nano_id` varchar(10) NOT NULL,
	`created_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
	`updated_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
	`value` UNSIGNED BIG INT NOT NULL DEFAULT 0,
	unique (`id`)
);
CREATE UNIQUE INDEX IF NOT EXISTS c_nano_id ON c(nano_id);
CREATE TRIGGER IF NOT EXISTS UPDATE_C BEFORE UPDATE ON c
    BEGIN
       UPDATE c SET updated_at = datetime('now', 'utc')
       WHERE rowid = new.rowid;
    END;

CREATE TABLE IF NOT EXISTS g (
	`id` integer NOT NULL PRIMARY KEY AUTOINCREMENT,
	`nano_id` varchar(10) NOT NULL,
	`created_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
	`updated_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
	`value` BIG INT NOT NULL DEFAULT 0,
	unique (`id`)
);
CREATE UNIQUE INDEX IF NOT EXISTS g_nano_id ON g(nano_id);
CREATE TRIGGER IF NOT EXISTS UPDATE_G BEFORE UPDATE ON g
    BEGIN
       UPDATE g SET updated_at = datetime('now', 'utc')
       WHERE rowid = new.rowid;
    END;
