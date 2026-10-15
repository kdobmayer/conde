// The thread answer (ia.md "Thread view", "owner.thread"): continuations merged by id, the tree
// built from `top`, a hidden parent carried through.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { emptyThread, mergeThread, treeOrder } from '../src/lib/thread-tree.ts';

const msg = (id, seq, reply_to, from = 'kdobmayer/desk') => ({
	message_id: id,
	seq,
	reply_to,
	from,
	date: `2026-10-14T12:${String(seq).padStart(2, '0')}:00Z`,
	text: `${id} text\nsecond line`
});

test('a thread with a hidden parent and a continuation has no holes', () => {
	// T replies to something the owner cannot read; A under T; B and C under A; D under C.
	const T = msg('T', 10, 'X'), A = msg('A', 11, 'T'), B = msg('B', 12, 'A'), C = msg('C', 13, 'A'), D = msg('D', 14, 'C');
	let t = mergeThread(emptyThread(), { top: 'T', start: null, hidden_parent: true, messages: [T, A, B], next: 12 });
	assert.deepEqual([t.hiddenParent, t.next], [true, 12]);
	t = mergeThread(t, { top: 'T', start: null, hidden_parent: true, messages: [A, C, D], next: null });
	const ids = t.messages.map((m) => m.message_id);
	assert.deepEqual(ids, ['T', 'A', 'B', 'C', 'D'], 'each once, by seq (A came twice)');
	for (const m of t.messages) assert.ok(m.message_id === t.top || ids.includes(m.reply_to), `${m.message_id}'s parent`);
	assert.equal(t.next, null);

	const tree = treeOrder(t.messages, t.top).map((l) => [l.message.message_id, l.depth, l.re?.line ?? null]);
	assert.deepEqual(tree, [
		['T', 0, null],
		['A', 0, null],
		['B', 1, null],
		['C', 1, 'A text'],
		['D', 1, null]
	]);
});
