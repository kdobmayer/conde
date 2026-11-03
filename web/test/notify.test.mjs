// An own agent's push setting per channel: the control asks for the other setting, names itself
// for a screen reader, and the rail takes the board's answer for that one agent and channel only.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { nextNotify, notifyHint, notifyLabel, withNotify } from '../src/lib/notify.ts';
import { conversationsOf } from '../src/lib/conversation.ts';

const ch = (channel, notify) => ({ channel, topic: null, last_seq: 1, last_at: null, unread: 0, notify });
const rail = () => ({
	owner: 'k',
	agents: [
		{ name: 'k/desk', kind: 'claude', address: null, dms: [], channels: [ch('#dev', 'mentions'), ch('#ops', 'all')] },
		{ name: 'k/astra', kind: 'codex', address: null, dms: [], channels: [ch('#dev', 'mentions')] }
	]
});

test('a press asks for the other setting', () => {
	assert.equal(nextNotify('mentions'), 'all');
	assert.equal(nextNotify('all'), 'mentions');
});

test('the control names the agent and the channel; the hint says what a press does', () => {
	assert.equal(notifyLabel('desk', '#dev'), 'Notify desk of every post in #dev');
	assert.match(notifyHint('mentions'), /mentions only\. Press for every post/);
	assert.match(notifyHint('all'), /every post\. Press for mentions only/);
});

test('the answer changes one agent in one channel, and leaves the rail it came from alone', () => {
	const before = rail();
	const after = withNotify(before, 'k/desk', '#dev', 'all');
	assert.deepEqual(after.agents[0].channels.map((c) => c.notify), ['all', 'all']);
	assert.equal(after.agents[1].channels[0].notify, 'mentions', 'another agent in the same channel keeps its setting');
	assert.equal(before.agents[0].channels[0].notify, 'mentions', 'the old rail is not mutated');
	assert.equal(after.agents[1], before.agents[1], 'untouched agents are kept as they were');
});

test('the agent page lists each channel with its setting', () => {
	const { channels } = conversationsOf(rail().agents[0]);
	assert.deepEqual(channels.map((c) => [c.target, c.notify]), [['#dev', 'mentions'], ['#ops', 'all']]);
});
