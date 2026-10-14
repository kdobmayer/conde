// Gate for U1: a conversation opens scrolled to its newest message.
// Usage: node scripts/design/probe-scroll.mjs [base-url]   (default http://127.0.0.1:5799, the mock)
// Opens the DM route headless at each size; after load, the message list's scrollTop must equal
// scrollHeight - clientHeight on a list that overflows. Exits 1 if any size fails.
// Needs playwright-core 1.64.0 (npm i --no-save) and a Chromium headless shell, as shoot.mjs.

import { homedir } from 'node:os';
import { chromium } from 'playwright-core';

const base = process.argv[2] ?? 'http://127.0.0.1:5799';
const url = `${base}/#/a/desk/dm/konstantin/kosztya-devops-manager`;
const executablePath =
	process.env.CHROME ??
	`${homedir()}/.cache/ms-playwright/chromium_headless_shell-1243/chrome-headless-shell-linux64/chrome-headless-shell`;
const sizes = {
	'1440x900': { width: 1440, height: 900 },
	'1180x820': { width: 1180, height: 820 },
	'390x844': { width: 390, height: 844 }
};

const browser = await chromium.launch({ executablePath, args: ['--disable-gpu'] });
let failed = false;
for (const [name, viewport] of Object.entries(sizes)) {
	const context = await browser.newContext({ viewport, reducedMotion: 'reduce' });
	const page = await context.newPage();
	page.on('pageerror', (e) => console.error(`${name}: page error: ${e.message}`));
	await page.goto(url, { waitUntil: 'networkidle' });
	await page.waitForFunction(() => {
		const s = document.querySelector('.scroll');
		return s && !s.textContent.includes('Loading…') && s.scrollHeight > 0;
	});
	await page.waitForTimeout(300);
	const m = await page.$eval('.scroll', (s) => ({ top: s.scrollTop, height: s.scrollHeight, client: s.clientHeight }));
	const end = m.height - m.client;
	const ok = end > 0 && Math.abs(m.top - end) <= 1;
	console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}: scrollTop ${m.top}, scrollHeight - clientHeight ${end}`);
	if (!ok) failed = true;
	await context.close();
}
await browser.close();
process.exit(failed ? 1 : 0);
