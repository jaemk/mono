-- show everyone the value of a card a failed match targeted (SET-13)
ALTER TABLE rooms ADD COLUMN show_misses BOOLEAN NOT NULL DEFAULT true;
