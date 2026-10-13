-- lifetime totals from the counters rooms already kept; the new counters,
-- match times, and partners start at zero (STAT-7). A player that already
-- has totals keeps them. The integration tests run this file on seeded rooms.
INSERT INTO player_stats (player_id, games_played, wins, komino_calls, komino_wins, matches,
                          failed_matches, special_moves, cards_interacted, forfeits)
SELECT s.player_id, sum(s.games_played), sum(s.wins), sum(s.komino_calls), sum(s.komino_wins),
       sum(s.matches), sum(s.failed_matches), sum(s.special_moves), sum(s.cards_interacted),
       sum(s.forfeits)
FROM room_stats s JOIN players p ON p.id = s.player_id
WHERE NOT p.bot
GROUP BY s.player_id
ON CONFLICT (player_id) DO NOTHING;
