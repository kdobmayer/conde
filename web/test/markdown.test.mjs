// Message bodies in the web app (lib/markdown.ts): the markdown subset, text that is not
// markdown kept as it was, names never read as emphasis, the sanitiser, and the plain text the
// previews show. DOMPurify runs over jsdom here, over the page's own DOM in the app.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { JSDOM } from 'jsdom';
import createDOMPurify from 'dompurify';
import { plainText, renderMarkdown } from '../src/lib/markdown.ts';

const purify = createDOMPurify(new JSDOM('').window);
const html = (text) => renderMarkdown(text, purify);
const LINK = (href, text) => `<a href="${href}" target="_blank" rel="noopener noreferrer">${text}</a>`;

test('paragraphs and line breaks', () => {
	assert.equal(html('one\ntwo\n\nthree'), '<p>one<br>two</p><p>three</p>');
});

test('bold, italic and inline code', () => {
	assert.equal(html('**b** __b__ *i* _i_ `c <d>`'), '<p><strong>b</strong> <strong>b</strong> <em>i</em> <em>i</em> <code>c &lt;d&gt;</code></p>');
});

test('fenced and indented code blocks, unhighlighted, content escaped', () => {
	assert.equal(html('```rust\nlet a = b_c * 2;\n  <x>\n```'), '<pre><code>let a = b_c * 2;\n  &lt;x&gt;\n</code></pre>');
	assert.equal(html('text\n\n    indented\n    block'), '<p>text</p><pre><code>indented\nblock\n</code></pre>');
});

test('ordered and unordered lists, nested', () => {
	assert.equal(html('- a\n- b\n  - c'), '<ul><li>a</li><li>b<ul><li>c</li></ul></li></ul>');
	assert.equal(html('3. three\n4. four'), '<ol start="3"><li>three</li><li>four</li></ol>');
	assert.equal(html('- [ ] open\n- [x] done'), '<ul><li>[ ] open</li><li>[x] done</li></ul>', 'a task box stays text, no form control');
});

test('blockquotes', () => {
	assert.equal(html('> quoted\n> on two lines'), '<blockquote><p>quoted<br>on two lines</p></blockquote>');
});

test('https links open in a new tab; bare https URLs are linked', () => {
	assert.equal(html('[docs](https://example.com/a)'), `<p>${LINK('https://example.com/a', 'docs')}</p>`);
	assert.equal(html('see https://example.com/x_y_z.'), `<p>see ${LINK('https://example.com/x_y_z', 'https://example.com/x_y_z')}.</p>`);
	assert.equal(html('<https://example.com>'), `<p>${LINK('https://example.com/', 'https://example.com')}</p>`);
});

test('other schemes, bare hosts and emails stay text', () => {
	assert.equal(html('[a](http://example.com) http://example.com www.example.com a@b.example'), '<p>a http://example.com www.example.com a@b.example</p>');
	assert.equal(html('[a](/relative) [m](mailto:a@b.example)'), '<p>a m</p>');
});

test('headings are bold paragraphs, never page headings', () => {
	assert.equal(html('# Title\n### Sub *x*'), '<p><strong>Title</strong></p><p><strong>Sub <em>x</em></strong></p>');
});

test('text that is not markdown renders as plain text did', () => {
	for (const [text, out] of [
		['Morning. Picking up B13 from the backlog.', '<p>Morning. Picking up B13 from the backlog.</p>'],
		['Two  spaces stay, and so\ndoes the line break.', '<p>Two  spaces stay, and so<br>does the line break.</p>'],
		['5 * 3 * 2 = 30, a_b_c, 2 < 3 > 1 & so on', '<p>5 * 3 * 2 = 30, a_b_c, 2 &lt; 3 &gt; 1 &amp; so on</p>'],
		['&amp; &lt;b&gt; &#38; stay as typed', '<p>&amp;amp; &amp;lt;b&amp;gt; &amp;#38; stay as typed</p>'],
		['| a | b |\n|---|---|\n| 1 | 2 |', '<p>| a | b |<br>|---|---|<br>| 1 | 2 |</p>'],
		['~~not struck~~ and ~one~', '<p>~~not struck~~ and ~one~</p>'],
		['Title\n===', '<p>Title<br>===</p>'],
		['[ref]: https://example.com\n[ref]', `<p>[ref]: ${LINK('https://example.com/', 'https://example.com')}<br>[ref]</p>`],
		['Jó hír: a tűzfalszabály kész, ő és ű.', '<p>Jó hír: a tűzfalszabály kész, ő és ű.</p>']
	])
		assert.equal(html(text), out, text);
});

test('agent names, mentions and #channels survive untouched', () => {
	for (const text of [
		'kdobmayer/dakai_helper asked konstantin/nuc_worker_2',
		'ping konstantin/_draft_ and kdobmayer/a_b_ now',
		'posted in #conde_dev_ and #_x_, cc @desk_bot_',
		'kdobmayer/project.task_a.role_b and team/x_y_z'
	])
		assert.equal(html(text), `<p>${text}</p>`, text);
	assert.equal(html('_see kdobmayer/a_b_ now_'), '<p><em>see kdobmayer/a_b_ now</em></p>', 'emphasis around a name still works');
	assert.equal(html('**kdobmayer/desk** in `#ops_x`'), '<p><strong>kdobmayer/desk</strong> in <code>#ops_x</code></p>');
});

test('sanitiser: raw HTML, scripts, images and unsafe links render as text or are dropped', () => {
	assert.equal(html('<script>alert(1)</script>'), '<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>');
	assert.equal(html('<img src=x onerror=alert(1)>'), '<p>&lt;img src=x onerror=alert(1)&gt;</p>');
	assert.equal(html('<div onclick="x()">\nblock\n</div>'), '<p>&lt;div onclick="x()"&gt;<br>block</p><p>&lt;/div&gt;</p>');
	assert.equal(html('![a cat](https://example.com/cat.png)'), '<p>a cat</p>', 'an image keeps only its alt text');
	assert.equal(html('[click](javascript:alert(1))'), '<p>click</p>');
	assert.equal(html('<javascript:alert(1)>'), '<p>javascript:alert(1)</p>');
	assert.equal(html('[x](data:text/html;base64,PHNjcmlwdD4=)'), '<p>x</p>');
	assert.equal(html('[x](JaVaScRiPt:alert(1))'), '<p>x</p>');
	assert.equal(
		html('[x](https://e.example/" onmouseover="alert(1))'),
		`<p>[x](${LINK('https://e.example/', 'https://e.example/')}" onmouseover="alert(1))</p>`,
		'a quote in a link target never becomes an attribute'
	);
	const out = html('[ok](https://example.com) <b>b</b> <iframe src="https://x"></iframe> <style>*{}</style>');
	assert.ok(!/<(b|iframe|style|img|script)\b/.test(out), out);
});

test('without a DOM the text comes back escaped, never as unsanitised HTML', () => {
	assert.equal(renderMarkdown('<b>x</b> **y**', { isSupported: false, sanitize: () => 'unsafe' }), '<p>&lt;b&gt;x&lt;/b&gt; **y**</p>');
});

test('previews: plain text with the markup removed, one line per block', () => {
	assert.equal(plainText('**Release** _today_: see [notes](https://example.com) and `cfg`'), 'Release today: see notes and cfg');
	assert.equal(plainText('# Plan\n\n- one\n- two\n\n```\ncode()\n```'), 'Plan\none\ntwo\ncode()');
	assert.equal(plainText('> quoted *line*'), 'quoted line');
	assert.equal(plainText('Steps:\n\n2. build\n3. *ship*'), 'Steps:\n2. build\n3. ship', 'an ordered item keeps its number');
	assert.equal(plainText('kdobmayer/dakai_helper in #ops_x'), 'kdobmayer/dakai_helper in #ops_x');
	assert.equal(plainText('Plain text, 5 * 3 & <b>.'), 'Plain text, 5 * 3 & <b>.');
	assert.equal(plainText('![chart](https://x.example/c.png)'), 'chart');
});
