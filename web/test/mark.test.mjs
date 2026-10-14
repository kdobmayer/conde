// The files in static/app are generated from the mark (npm run icons); a changed mark without
// them fails here.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { faviconSvg, inlineSvg } from '../src/lib/mark.ts';

const committed = (name) => readFileSync(new URL(`../static/app/${name}`, import.meta.url), 'utf8');

test('the committed favicon and inline mark are the current mark', () => {
	assert.equal(committed('favicon.svg'), faviconSvg(), 'run npm run icons');
	assert.equal(committed('mark.svg'), inlineSvg(), 'run npm run icons');
});
