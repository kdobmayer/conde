// The one delivery word of a message (ia.md "Feed"): the worst recipient state; problems first.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { stateWord } from '../src/lib/ui/state.ts';

const dm = (...states) => ({ place: 'dm', delivery: { recipients: states.map((state, i) => ({ recipient: `r${i}`, state })), read_by: null } });
const post = (states, read_by) => ({ place: '#infra', delivery: { recipients: states.map((state, i) => ({ recipient: `r${i}`, state })), read_by } });

test('each recipient state has its word and tone', () => {
	const table = Object.fromEntries(
		['read', 'delivered', 'notified', 'unread', 'queued', 'sending', 'sent', 'retrying', 'held', 'denied', 'refused', 'gone'].map((s) => {
			const w = stateWord(dm(s));
			return [s, `${w.word}/${w.tone}`];
		})
	);
	assert.deepEqual(table, {
		read: 'read/done',
		delivered: 'delivered/done',
		notified: 'delivered/done',
		unread: 'delivered/done',
		queued: 'queued/pending',
		sending: 'sending/pending',
		sent: 'sent/pending',
		retrying: 'retrying/problem',
		held: 'held/problem',
		denied: 'denied/problem',
		refused: 'refused/problem',
		gone: 'gone/problem'
	});
});

test('the worst recipient wins, and a problem beats a channel read count', () => {
	assert.deepEqual(stateWord(dm('read', 'retrying', 'delivered')), { word: 'retrying', tone: 'problem' });
	assert.deepEqual(stateWord(dm('held', 'gone')), { word: 'gone', tone: 'problem' });
	assert.deepEqual(stateWord(dm('read', 'delivered')), { word: 'delivered', tone: 'done' });
	assert.deepEqual(stateWord(dm('read', 'queued')), { word: 'queued', tone: 'pending' });
	assert.deepEqual(stateWord(post(['held'], { read: 3, total: 4 })), { word: 'held', tone: 'problem' });
	assert.deepEqual(stateWord(post(['delivered'], { read: 3, total: 4 })), { word: 'read 3 of 4', tone: 'done' });
});

test('a channel post with nothing known is posted; a DM with nothing known has no word', () => {
	assert.deepEqual(stateWord({ place: '#infra' }), { word: 'posted', tone: 'done' });
	assert.deepEqual(stateWord(post([], null)), { word: 'posted', tone: 'done' });
	assert.equal(stateWord({ place: 'dm' }), null);
	assert.equal(stateWord(dm('mystery')), null, 'an unknown state is not guessed at');
});
