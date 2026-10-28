-- What a dump and restore must carry over unchanged, one "key|value" line each (psql -At): the row count of every
-- table, the newest message, every sequence position and the schema version. Run on both sides and diff.
SELECT format('SELECT %L, count(*) FROM public.%I', 'rows:' || tablename, tablename) FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename \gexec
SELECT 'messages:max_commit_seq', max(commit_seq) FROM public.messages;
SELECT 'messages:max_message_id', max(message_id) FROM public.messages;
SELECT 'seq:' || sequencename, last_value FROM pg_sequences WHERE schemaname = 'public' ORDER BY sequencename;
SELECT 'schema:max_migration', max(version) FROM public.schema_migrations;
