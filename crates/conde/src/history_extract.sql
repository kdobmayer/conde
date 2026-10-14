-- H1 extraction (DESIGN-v2 section 10): the rows of a Postgres board that involve one owner,
-- as metadata and sha256 hashes only: no message text, no attachment bytes, no address target.
-- One JSON array per line, prefixed 'H1 ', then one 'end' line with the freeze point (the board's
-- max commit_seq), the line count and the sha256 of the sorted lines joined by '\n'.
-- Run read-only in one snapshot, the owner in the setting conde.owner:
--   BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; SET LOCAL conde.owner = '<owner>'; <this>; COMMIT;
-- Scope: the owner's agents; every channel one of them is a member of, posted in or created; every
-- message sent or received by one of them or posted in one of those channels; every member of those
-- channels (any owner) and every agent those rows name.
WITH k AS (
    SELECT user_id FROM users WHERE name = current_setting('conde.owner')
), ka AS (
    SELECT agent_id FROM agents WHERE owner_id = (SELECT user_id FROM k)
), ch AS (
    SELECT channel_id FROM memberships WHERE agent_id IN (SELECT agent_id FROM ka)
    UNION SELECT channel_id FROM messages WHERE channel_id IS NOT NULL AND sender_agent_id IN (SELECT agent_id FROM ka)
    UNION SELECT channel_id FROM channels WHERE created_by IN (SELECT agent_id FROM ka)
), m AS (
    SELECT * FROM messages
    WHERE sender_agent_id IN (SELECT agent_id FROM ka)
       OR recipient_agent_id IN (SELECT agent_id FROM ka)
       OR channel_id IN (SELECT channel_id FROM ch)
), mem AS (
    SELECT * FROM memberships WHERE channel_id IN (SELECT channel_id FROM ch)
), rx AS (
    SELECT * FROM reactions WHERE message_id IN (SELECT message_id FROM m)
), dr AS (
    SELECT * FROM dm_read WHERE message_id IN (SELECT message_id FROM m)
), ag AS (
    SELECT agent_id FROM ka
    UNION SELECT sender_agent_id FROM m
    UNION SELECT recipient_agent_id FROM m WHERE recipient_agent_id IS NOT NULL
    UNION SELECT agent_id FROM mem
    UNION SELECT actor_id FROM rx
    UNION SELECT agent_id FROM dr
    UNION SELECT created_by FROM channels WHERE channel_id IN (SELECT channel_id FROM ch)
), lines AS (
    SELECT json_build_array('agent', a.agent_id, u.name, a.name_lower, a.address_kind, a.host_id,
                            a.pinned_address IS NOT NULL, a.binding_version)::text AS l
    FROM agents a JOIN users u ON u.user_id = a.owner_id WHERE a.agent_id IN (SELECT agent_id FROM ag)
    UNION ALL
    SELECT json_build_array('channel', c.channel_id, c.name_lower, c.created_by)::text
    FROM channels c WHERE c.channel_id IN (SELECT channel_id FROM ch)
    UNION ALL
    SELECT json_build_array('member', channel_id, agent_id, read_seq)::text FROM mem
    UNION ALL
    SELECT json_build_array('msg', message_id, commit_seq, sender_agent_id, recipient_agent_id, channel_id,
                            reply_to, created_at, encode(sha256(convert_to(text, 'UTF8')), 'hex'),
                            octet_length(convert_to(text, 'UTF8')))::text
    FROM m
    UNION ALL
    SELECT json_build_array('att', attachment_id, message_id, filename, size, octet_length(bytes),
                            encode(sha256(bytes), 'hex'))::text
    FROM attachments WHERE message_id IN (SELECT message_id FROM m)
    UNION ALL
    SELECT json_build_array('read', message_id, agent_id, read_at)::text FROM dr
    UNION ALL
    SELECT json_build_array('react', message_id, actor_id, emoji, present)::text FROM rx
    UNION ALL
    SELECT json_build_array('host', h.host_id,
                            EXISTS (SELECT 1 FROM device_credentials d WHERE d.host_id = h.host_id AND d.revoked_at IS NULL))::text
    FROM hosts h WHERE h.user_id = (SELECT user_id FROM k)
)
SELECT 'H1 ' || l FROM lines
UNION ALL
SELECT 'H1 ' || json_build_array('end', current_setting('conde.owner'), (SELECT max(commit_seq) FROM messages),
                                 (SELECT count(*) FROM messages), now()::text, count(*),
                                 encode(sha256(convert_to(coalesce(string_agg(l, E'\n' ORDER BY l COLLATE "C"), ''), 'UTF8')), 'hex'))::text
FROM lines;
