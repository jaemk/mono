-- per-room game settings (SET-3); null means no turn limit, and peeks until hidden
ALTER TABLE rooms
    ADD COLUMN hand_size INT NOT NULL DEFAULT 4,
    ADD COLUMN away_grace_secs INT NOT NULL DEFAULT 30,
    ADD COLUMN turn_limit_secs INT,
    ADD COLUMN reveal_secs INT;
