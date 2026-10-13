DROP INDEX rooms_host;
DROP INDEX room_stats_player;
DROP INDEX room_members_player;
DROP INDEX players_anonymous_seen;
ALTER TABLE players DROP COLUMN recovery_hash;
