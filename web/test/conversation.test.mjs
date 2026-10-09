// Pagination survives an initial history request overtaken by a refresh (review repro
// web-pagination, inverted), on the component's own script and on the thread helpers.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { test } from 'node:test';
import { acceptEarlier, acceptNewest, emptyThread } from '../src/lib/thread.ts';

const page = (seqs, more) => ({ messages: seqs.map((seq) => ({ seq, message_id: `m${seq}` })), more, unread: 0 });

test('the first accepted newest page sets "more", whichever request asked', () => {
	let t = acceptNewest(emptyThread(), page([101], true));
	assert.equal(t.more, true);
	t = acceptNewest(t, page([101, 102], false));
	assert.equal(t.more, true, 'a later newest page does not hide earlier messages');
	t = acceptEarlier(t, page([1, 2], false));
	assert.deepEqual(t.messages.map((m) => m.seq), [1, 2, 101, 102]);
	assert.equal(t.more, false);
});

test('Conversation keeps "Load earlier" when its initial request is superseded', async () => {
	const source = readFileSync(new URL('../src/lib/Conversation.svelte', import.meta.url), 'utf8');
	const script = source
		.split('<script lang="ts">')[1]
		.split('</script>')[0]
		.replace(/^\s*import .*;$/gm, '');
	const pending = [];
	const rpc = () => new Promise((resolve) => pending.push(resolve));
	const body = stripTypeScriptTypes(script) + '\nreturn {refresh, snapshot: () => ({more, loading, messages})};';
	const names = ['$props', '$state', '$derived', '$effect', 'rpc', 'document', 'tick', 'short', 'changed', 'onBoard', 'RpcError'];
	const c = new Function(...names, 'acceptNewest', 'acceptEarlier', 'emptyThread', body)(
		() => ({ target: 'owner/worker' }),
		(x) => x,
		(x) => x,
		() => {},
		rpc,
		{ visibilityState: 'visible' },
		async () => {},
		(x) => x,
		() => {},
		() => () => {},
		class extends Error {},
		acceptNewest,
		acceptEarlier,
		emptyThread
	);
	const initial = c.refresh(true);
	const refreshed = c.refresh();
	const answer = page([101], true);
	pending[1](answer);
	await refreshed;
	pending[0](answer);
	await initial;
	assert.equal(c.snapshot().more, true);
	assert.equal(c.snapshot().loading, false);
});
