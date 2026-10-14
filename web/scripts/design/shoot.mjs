// Design-only: headless screenshots of pages on a local server, one at a time.
// Usage: node scripts/design/shoot.mjs <shots.json>
// shots.json: [{ "url": "...", "out": "path.png", "size": "desktop" | "phone", "scheme": "light" | "dark",
//                "wait"?: "css selector", "click"?: "css selector" }]
// Needs playwright-core 1.64.0 (npm i --no-save) and a Chromium headless shell under
// ~/.cache/ms-playwright (CHROME overrides the path). The clock is fixed to the fixture's NOW
// in Europe/Budapest so relative times and day labels are stable.

import { readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { chromium } from 'playwright-core';

const shots = JSON.parse(readFileSync(process.argv[2], 'utf8'));
const executablePath =
	process.env.CHROME ??
	`${homedir()}/.cache/ms-playwright/chromium_headless_shell-1243/chrome-headless-shell-linux64/chrome-headless-shell`;
const NOW = new Date('2026-10-14T12:20:00Z');
const sizes = {
	desktop: { viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 },
	phone: { viewport: { width: 390, height: 844 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true }
};

const browser = await chromium.launch({ executablePath, args: ['--disable-gpu', '--font-render-hinting=none'] });
for (const s of shots) {
	const context = await browser.newContext({
		...sizes[s.size],
		colorScheme: s.scheme,
		locale: 'en-GB',
		timezoneId: 'Europe/Budapest',
		reducedMotion: 'reduce'
	});
	const page = await context.newPage();
	await page.clock.setFixedTime(NOW);
	page.on('pageerror', (e) => console.error(`${s.out}: page error: ${e.message}`));
	await page.goto(s.url, { waitUntil: 'networkidle' });
	if (s.click) await page.click(s.click);
	if (s.wait) await page.waitForSelector(s.wait);
	await page.waitForTimeout(400);
	await page.screenshot({ path: s.out });
	console.log(`wrote ${s.out}`);
	await context.close();
}
await browser.close();
