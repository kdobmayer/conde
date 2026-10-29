// The agent page's list width (lib/list-width.ts): read and written per browser, kept between
// 200 and 480 px, the default when nothing usable is stored or storage throws.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { WIDTH_KEY, asWidth, readWidth, writeWidth } from '../src/lib/list-width.ts';

const store = () => {
	const m = new Map();
	return { m, getItem: (k) => m.get(k) ?? null, setItem: (k, v) => m.set(k, v), removeItem: (k) => m.delete(k) };
};
const throwing = { getItem: () => { throw new Error('blocked'); }, setItem: () => { throw new Error('blocked'); }, removeItem: () => { throw new Error('blocked'); } };

test('a stored width is read back inside 200 to 480 px; anything else is the default', () => {
	assert.deepEqual(['320', '100', '900', '312.6', null, '', 'wide', 'NaN'].map(asWidth), [320, 200, 480, 313, null, null, null, null]);
	assert.equal(readWidth(throwing), null);
	assert.equal(readWidth(undefined), null);
});

test('writing stores the clamped width; null forgets it; blocked storage is ignored', () => {
	const s = store();
	writeWidth(999, s);
	assert.equal(s.m.get(WIDTH_KEY), '480');
	assert.equal(readWidth(s), 480);
	writeWidth(null, s);
	assert.equal(s.m.has(WIDTH_KEY), false);
	assert.doesNotThrow(() => writeWidth(300, throwing));
});
