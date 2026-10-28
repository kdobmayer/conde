// The conversation (plan U10): pagination survives an overtaken first request, reply context
// shows only when the parent is not the message above, and each message's one delivery word is
// U8's mapping with the detail the DM page adds.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { test } from 'node:test';
import { acceptEarlier, acceptNewest, emptyThread } from '../src/lib/thread.ts';
import { channelAgent, deliveryWord, firstLine, latestOf, replyContext } from '../src/lib/conversation.ts';
import { stateWord } from '../src/lib/ui/state.ts';
import { clock, countdown } from '../src/lib/time.ts';

const msg = (seq, reply_to = null) => ({ seq, message_id: `m${seq}`, reply_to, from: `k/a${seq}`, text: `text ${seq}\nmore` });
const page = (seqs, more) => ({ messages: seqs.map((seq) => msg(seq)), more, unread: 0 });

/** Conversation.svelte's script, run outside Svelte with its imports injected. */
function component(rpc) {
	const source = readFileSync(new URL('../src/lib/Conversation.svelte', import.meta.url), 'utf8');
	const script = source
		.split('<script lang="ts">')[1]
		.split('</script>')[0]
		.replace(/^\s*import .*;$/gm, '');
	const body = stripTypeScriptTypes(script) + '\nreturn {refresh, parentOf, snapshot: () => ({more, loading, messages})};';
	const derived = (x) => x;
	derived.by = (f) => f;
	const inject = {
		$props: () => ({ agent: 'k/worker', target: 'david/scout' }),
		$state: (x) => x,
		$derived: derived,
		$effect: () => {},
		rpc,
		tick: async () => {},
		short: (x) => x,
		board: { rail: null, fleet: null, session: null },
		onBoard: () => () => {},
		RpcError: class extends Error {},
		sameOwner: () => true,
		KIND: {},
		firstLine,
		replyContext,
		day: () => 'Today',
		acceptNewest,
		acceptEarlier,
		emptyThread
	};
	return new Function(...Object.keys(inject), body)(...Object.values(inject));
}

test('the first accepted newest page sets "more", whichever request asked', () => {
	let t = acceptNewest(emptyThread(), page([101], true));
	assert.equal(t.more, true);
	t = acceptNewest(t, page([101, 102], false));
	assert.equal(t.more, true, 'a later newest page does not hide earlier messages');
	t = acceptEarlier(t, page([1, 2], false));
	assert.deepEqual(t.messages.map((m) => m.seq), [1, 2, 101, 102]);
	assert.equal(t.more, false);
});

test('Conversation keeps "earlier" when its initial request is superseded', async () => {
	const pending = [];
	const c = component(() => new Promise((resolve) => pending.push(resolve)));
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

test('reply context only when the parent is not the message just above', () => {
	const list = [msg(1), msg(2, 'm1'), msg(3, 'm1'), msg(4), msg(5, 'm0')];
	assert.deepEqual(
		list.map((_, i) => replyContext(list, i)),
		[null, null, 'm1', null, 'm0'],
		'a reply to the message above has none; to an earlier one or one not loaded, the parent'
	);
	assert.equal(replyContext([msg(7, 'm6')], 0), 'm6', 'the first loaded message names its parent');
});

test('Conversation gives each message the rule\'s parent: its first line when loaded, else "earlier"', async () => {
	const answer = { messages: [msg(1), msg(2, 'm1'), msg(3, 'm1'), msg(4, 'm0')], more: false };
	const c = component(async () => answer);
	await c.refresh(true);
	assert.deepEqual(
		[0, 1, 2, 3].map((i) => c.parentOf(i)),
		[null, null, { from: 'a1', line: 'text 1' }, 'earlier']
	);
});

const dm = (...recipients) => ({ place: 'dm', delivery: { recipients, read_by: null } });

test('the delivery word is U8\'s mapping for every state', () => {
	const now = Date.parse('2026-10-14T12:20:00Z');
	for (const state of ['read', 'delivered', 'notified', 'unread', 'queued', 'sending', 'sent', 'retrying', 'held', 'denied', 'refused', 'gone']) {
		const m = dm({ recipient: 'k/x', state });
		const w = deliveryWord(m, now);
		const u8 = stateWord(m);
		assert.deepEqual({ word: w.word, tone: w.tone }, u8, state);
	}
	const post = { place: '#infra', delivery: { recipients: [], read_by: { read: 3, total: 4 } } };
	assert.deepEqual(deliveryWord(post, now), { ...stateWord(post), detail: null });
	assert.equal(deliveryWord({ place: 'dm', delivery: { recipients: [], read_by: null } }, now), null, 'a DM with nothing known says nothing');
});

test('the delivery word carries its detail: read time, retry clock, failure reason', () => {
	const now = Date.parse('2026-10-14T12:20:00Z');
	const readAt = '2026-10-14T10:49:00Z';
	assert.equal(deliveryWord(dm({ recipient: 'k/x', state: 'read', read_at: readAt }), now).detail, clock(readAt));
	assert.equal(
		deliveryWord(dm({ recipient: 'k/x', state: 'read', read_at: readAt }, { recipient: 'k/y', state: 'read', read_at: readAt }), now).detail,
		null,
		'several readers: the time is in the detail view'
	);
	const retry = '2026-10-14T12:20:23Z';
	const w = deliveryWord(dm({ recipient: 'k/x', state: 'read' }, { recipient: 'k/y', state: 'retrying', retry_at: retry, attempts: 3 }), now);
	assert.deepEqual(w, { word: 'retrying', tone: 'problem', detail: countdown(retry, now) });
	assert.equal(w.detail, 'in 0:23');
	assert.equal(deliveryWord(dm({ recipient: 'k/x', state: 'refused', last_error: { status: 'refused', reason: 'not accepting' } }), now).detail, 'not accepting');
	assert.equal(deliveryWord(dm({ recipient: 'k/x', state: 'gone', last_error: { status: 'unreachable', reason: null } }), now).detail, 'unreachable');
	assert.equal(deliveryWord(dm({ recipient: 'k/x', state: 'held' }), now).detail, null);
});

test('Delivery renders U8\'s StateWord with the detail, the id only behind the disclosure', () => {
	const source = readFileSync(new URL('../src/lib/Delivery.svelte', import.meta.url), 'utf8');
	assert.match(source, /<StateWord \{message\} \/>/);
	assert.match(source, /deliveryWord\(message,/);
	const markup = source.split('</script>')[1].split('<style>')[0];
	const disclosure = markup.slice(markup.indexOf('<Disclosure>'), markup.indexOf('</Disclosure>'));
	assert.match(disclosure, /\{message\.message_id\}/, 'the id is inside the disclosure');
	assert.equal(markup.split('message.message_id').length - 1, 1, 'and nowhere else');
	const message = readFileSync(new URL('../src/lib/Message.svelte', import.meta.url), 'utf8').split('</script>')[1];
	assert.doesNotMatch(message.split('<style>')[0], /(?<!\$)\{message\.message_id\}/, 'Message never prints the id');
});

test('a channel is read through the first own member; an agent opens its latest conversation', () => {
	const agent = (name, dms, channels) => ({ name, kind: null, address: null, dms, channels });
	const ch = (channel, last_seq) => ({ channel, topic: null, last_seq, last_at: null, unread: 0 });
	const rail = {
		owner: 'k',
		agents: [agent('k/a', [], [ch('#x', 5)]), agent('k/b', [{ with: 'd/y', last_seq: 9, last_at: '', unread: 0 }], [ch('#x', 5), ch('#z', 7)])]
	};
	assert.equal(channelAgent(rail, '#x'), 'k/a');
	assert.equal(channelAgent(rail, '#z'), 'k/b');
	assert.equal(channelAgent(rail, '#none'), null);
	assert.equal(latestOf(rail.agents[1]), 'd/y');
	assert.equal(latestOf(rail.agents[0]), '#x');
	assert.equal(firstLine('\n  first line here\nsecond', 8), 'first l…');
});
