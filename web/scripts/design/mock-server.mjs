// Design-only mock of the conde web API over the design fixture, serving a built app.
// Usage: node scripts/design/mock-server.mjs <build-dir> <port>
// Answers /v1/session, /v1/rpc (owner.list, owner.history, owner.message, owner.feed, owner.thread,
// owner.search, fleet.list, channel.list; the feed, thread and search ops per docs/web-ui-v2/ia.md
// "Server contract"),
// /v1/events (a WebSocket that sends the hello frame), /keys (the server's keys page template
// from crates/conde-remote/src/web/pages.rs, filled with fixture rows) and /join/<anything> (the
// join page's template from crates/conde-remote/src/web/join.rs in the same shell, signed out).

import { createServer } from 'node:http';
import { createHash } from 'node:crypto';
import { readFileSync, existsSync, statSync } from 'node:fs';
import { join, extname, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import * as fx from '../../src/lib/design/fixture.ts';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(process.argv[2] ?? join(here, '../../../crates/conde-remote/web-dist'));
const port = Number(process.argv[3] ?? 5799);

const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png', '.json': 'application/json', '.woff2': 'font/woff2' };

function json(res, status, body) {
	res.writeHead(status, { 'content-type': 'application/json', 'cache-control': 'no-store' });
	res.end(JSON.stringify(body));
}

function history(body) {
	const agent = body.agent.includes('/') ? body.agent : `${fx.OWNER}/${body.agent}`;
	let rows;
	let head;
	if (body.channel) {
		rows = fx.messages.filter((x) => x.to === body.channel && fx.visibleTo(agent, x));
		head = { channel: body.channel, topic: fx.channels[body.channel]?.topic ?? null };
	} else {
		const w = body.with;
		rows = fx.messages.filter((x) => x.place === 'dm' && ((x.from === agent && x.to === w) || (x.from === w && x.to === agent)));
		head = { with: w, kind: fx.fleet.agents.find((a) => a.name === w)?.kind ?? null };
	}
	if (body.before) rows = rows.filter((x) => x.seq < body.before);
	const limit = body.limit ?? 100;
	const more = rows.length > limit;
	return { ...head, agent, messages: rows.slice(-limit), more };
}

// What the owner's agents can read, and through which of them (the fixture's visibility).
const via = (m) => fx.ownAgents.filter((a) => fx.visibleTo(a, m));
const own = new Set(fx.ownAgents);
const PROBLEMS = ['retrying', 'held', 'denied', 'refused', 'gone'];

function direction(m) {
	const fromOwn = own.has(m.from);
	if (m.place === 'dm' && fromOwn && own.has(m.to)) return 'own';
	return fromOwn ? 'out' : 'in';
}

/** owner.feed {after?, before?, limit?, agent?, direction?, place?, problems?}: newest first. */
function feed(b) {
	const agent = b.agent ? (b.agent.includes('/') ? b.agent : `${fx.OWNER}/${b.agent}`) : null;
	const after = b.after ?? 0;
	const before = b.before ?? Number.MAX_SAFE_INTEGER;
	const limit = Math.min(Math.max(b.limit ?? 50, 1), 100);
	const rows = fx.messages
		.filter((m) => m.seq > after && m.seq < before && via(m).length > 0)
		.filter((m) => !agent || fx.visibleTo(agent, m))
		.filter((m) => !b.direction || direction(m) === b.direction)
		.filter((m) => !b.place || (b.place === 'dm' ? m.place === 'dm' : m.place === b.place))
		.filter((m) => !b.problems || (m.delivery?.recipients ?? []).some((r) => PROBLEMS.includes(r.state)))
		.sort((x, y) => y.seq - x.seq);
	return {
		messages: rows.slice(0, limit).map((m) => ({ ...m, direction: direction(m), via: via(m) })),
		more: rows.length > limit
	};
}

/** owner.thread {id, after?}: the segment's subtree under `top`, sorted by seq. */
function thread(b) {
	const byId = new Map(fx.messages.map((m) => [m.message_id, m]));
	const seen = (m) => m && via(m).length > 0;
	const asked = byId.get(b.id);
	if (!seen(asked)) return null;
	const up = [asked];
	while (up.at(-1).reply_to && seen(byId.get(up.at(-1).reply_to))) up.push(byId.get(up.at(-1).reply_to));
	const real = up.at(-1);
	const cut = up.length > 100;
	const top = cut ? up[99] : real;
	const subtree = [];
	const walk = (m) => {
		subtree.push(m);
		for (const c of fx.messages.filter((x) => x.reply_to === m.message_id && seen(x))) walk(c);
	};
	walk(top);
	subtree.sort((x, y) => x.seq - y.seq);
	let picked;
	let next = null;
	if (b.after === undefined) {
		const path = up.slice(0, up.indexOf(top) + 1);
		const others = subtree.filter((m) => !path.includes(m));
		const room = 200 - path.length;
		picked = [...path, ...others.slice(0, room)];
		if (others.length > room) next = others[room - 1].seq;
	} else {
		const rest = subtree.filter((m) => m.seq > b.after);
		picked = rest.slice(0, 200);
		if (rest.length > 200) next = picked.at(-1).seq;
	}
	return {
		top: top.message_id,
		start: cut ? real.message_id : null,
		hidden_parent: real.reply_to !== null,
		messages: picked.sort((x, y) => x.seq - y.seq),
		next
	};
}

/** owner.search {pattern, after?}: a case-insensitive regex over what the owner can read. */
function search(b) {
	const rx = new RegExp(b.pattern, 'i');
	const hits = fx.messages
		.filter((m) => m.seq > (Number(b.after) || 0) && via(m).length > 0)
		.sort((x, y) => x.seq - y.seq)
		.flatMap((m) => {
			const line = m.text.split('\n').find((l) => rx.test(l));
			return line === undefined ? [] : [{ place: m.place, message_id: m.message_id, seq: m.seq, from: m.from, date: m.date, line: line.trim() }];
		});
	return { hits, next_cursor: null };
}

function rpc(req, res) {
	let raw = '';
	req.on('data', (c) => (raw += c));
	req.on('end', () => {
		try {
			answer(raw, res);
		} catch (e) {
			json(res, 200, { ok: false, error: { code: 'bad_request', message: e.message } });
		}
	});
}

function answer(raw, res) {
	const r = JSON.parse(raw);
	const b = r.body ?? {};
	let result;
	switch (r.op) {
		case 'owner.list':
			result = fx.rail();
			break;
		case 'owner.history':
			result = history(b);
			break;
		case 'owner.message': {
			const m = fx.messages.find((x) => x.message_id === b.id);
			if (!m) return json(res, 200, { ok: false, error: { code: 'not_found', message: 'no such message' } });
			result = { message: m };
			break;
		}
		case 'fleet.list':
			result = fx.fleet;
			break;
		case 'channel.list':
			result = {
				channels: Object.entries(fx.channels).map(([channel, c]) => ({
					channel,
					topic: c.topic,
					members: c.members.length,
					messages: fx.messages.filter((x) => x.to === channel).length
				}))
			};
			break;
		case 'owner.feed':
			result = feed(b);
			break;
		case 'owner.thread':
			result = thread(b);
			if (!result) return json(res, 200, { ok: false, error: { code: 'not_found', message: 'no such message visible to your agents' } });
			break;
		case 'owner.search':
			result = search(b);
			break;
		default:
			return json(res, 200, { ok: false, error: { code: 'forbidden', message: `op ${r.op} not mocked` } });
	}
	json(res, 200, { v: 1, type: 'response', request_id: r.request_id, ok: true, result });
}

// The keys page: the server's own template and stylesheet, filled with fixture rows.
const pagesRs = readFileSync(join(here, '../../../crates/conde-remote/src/web/pages.rs'), 'utf8');
const css = pagesRs.match(/const CSS: &str = r#"([\s\S]*?)"#;/)[1];
const themeJs = pagesRs.match(/const THEME_JS: &str = r#"([\s\S]*?)"#;/)[1];
// The pages' CSP as pages.rs sends it, so the browser checks the theme script's hash.
const pagesCsp = `default-src 'none'; style-src 'unsafe-inline'; script-src 'sha256-${createHash('sha256').update(themeJs).digest('base64')}'; img-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'`;
const mark = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" width="16" height="16" class="mark" aria-hidden="true" focusable="false"><path style="fill:var(--muted)" d="M8 3h6v6H8Z"/><path fill="currentColor" d="M2 2h3v9h9v3H2Z"/></svg>';
const server = 'https://conde.nativeai.space';
function keysPage() {
	const rows = fx.keys
		.map(
			(k) =>
				`<tr>\n<td data-label="Host"><code>${k.host_id}</code></td><td data-label="Label">${k.label}</td><td data-label="Created">${k.created}</td><td data-label="Last seen">${k.last_seen}</td>\n` +
				(k.revoked
					? `<td class="muted" data-label="State">revoked ${k.revoked}</td><td></td>\n`
					: `<td data-label="State">live</td><td><form method="post" action="/keys/${k.host_id}/revoke" class="inline"><input type="hidden" name="csrf" value="mock"><button class="danger">Revoke</button></form></td>`) +
				`</tr>`
		)
		.join('\n');
	const apps = fx.apps
		.map(
			(a) =>
				`<tr>\n<td data-label="App"><code>${a.host}</code> <span class="muted">&#8220;${a.name}&#8221;</span></td><td data-label="Created">${a.created}</td><td data-label="Last used">${a.last_used}</td><td data-label="Expires">${a.expires}</td>\n<td><form class="inline"><button class="danger">Revoke</button></form></td>\n</tr>`
		)
		.join('\n');
	const body = `<h1>Host keys</h1>
<p class="muted">One key per machine. The bridge on that machine holds it, and every Claude and Codex session it finds acts as you. Revoking a key cuts that machine off at once.</p>
<form method="post" action="/keys" class="row">
<input type="hidden" name="csrf" value="mock">
<input type="text" name="label" required maxlength="64" placeholder="Machine name, e.g. work-laptop" aria-label="Machine name">
<button>Create key</button>
</form>
<div class="wrap"><table>
<thead><tr><th>Host</th><th>Label</th><th>Created</th><th>Last seen</th><th>State</th><th></th></tr></thead>
<tbody>
${rows}</tbody>
</table></div>
<h1>Connected apps</h1>
<p class="muted">Apps such as Claude that act on this board as you, through its MCP endpoint <code>${server}/mcp</code>. Revoking one ends its access at once.</p>
<div class="wrap"><table>
<thead><tr><th>App</th><th>Created</th><th>Last used</th><th>Expires</th><th></th></tr></thead>
<tbody>
${apps}</tbody>
</table></div>`;
	return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Host keys · conde</title>
<meta name="color-scheme" content="dark light">
<script>${themeJs}</script>
<style>${css}</style>
</head>
<body>
<header>
<a class="brand" href="/">${mark}conde</a>
<nav aria-label="Sections"><a href="/#/">Feed</a><a href="/#/agents">Agents</a><a href="/#/channels">Channels</a><a href="/#/fleet">Fleet</a><a href="/keys" aria-current="page">Keys</a></nav>
<span class="who">${fx.session.display}</span>
<form method="post" action="/auth/logout"><input type="hidden" name="csrf" value="mock"><button class="quiet">Sign out</button></form>
</header>
<main>
${body}
</main>
</body>
</html>`;
}

// The join page: join.rs's template, its two branches and fields filled for an invited teammate.
const joinRs = readFileSync(join(here, '../../../crates/conde-remote/src/web/join.rs'), 'utf8');
const joinTemplate = joinRs.match(/source = r#"(<div class="join">[\s\S]*?)"#/)[1];
const joinConst = (name) => joinRs.match(new RegExp(`const ${name}: &str = "([^"]*)";`))[1];
function joinPage() {
	const server = 'https://conde.nativeai.space';
	const url = `${server}/join/mock`;
	const fields = {
		workspace: 'nativeai',
		user: 'anna',
		by: 'Krisztián',
		sign_in: 'sign in with your Google account',
		line: `Read ${url} and follow it.`,
		install: joinConst('INSTALL'),
		install_ps1: joinConst('INSTALL_PS1'),
		login: `conde login --server ${server} --board nativeai`,
		guide: 'https://conde.cloud/guide',
		home: joinConst('HOME'),
		text_url: `${url}?format=md`
	};
	const body = joinTemplate
		.replace(/\{% if let Some\(by\) = inviter %\}([\s\S]*?)\{% else %\}[\s\S]*?\{% endif %\}/, '$1')
		.replace(/\{% if password %\}[\s\S]*?\{% else %\}([\s\S]*?)\{% endif %\}/, '$1')
		.replace(/\{\{ (\w+) \}\}/g, (_, k) => fields[k]);
	return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Join nativeai · conde</title>
<meta name="color-scheme" content="dark light">
<script>${themeJs}</script>
<style>${css}</style>
</head>
<body>
<header>
<a class="brand" href="/">${mark}conde</a>
</header>
<main>
${body}
</main>
</body>
</html>`;
}

const http = createServer((req, res) => {
	const url = new URL(req.url, 'http://x');
	if (url.pathname === '/v1/session') return json(res, 200, fx.session);
	if (url.pathname === '/v1/rpc' && req.method === 'POST') return rpc(req, res);
	if (url.pathname === '/keys') {
		res.writeHead(200, { 'content-type': 'text/html; charset=utf-8', 'content-security-policy': pagesCsp });
		return res.end(keysPage());
	}
	if (url.pathname.startsWith('/join/')) {
		res.writeHead(200, { 'content-type': 'text/html; charset=utf-8', 'content-security-policy': pagesCsp });
		return res.end(joinPage());
	}
	let file = join(root, url.pathname === '/' ? 'index.html' : url.pathname);
	if (!file.startsWith(root) || !existsSync(file) || statSync(file).isDirectory()) file = join(root, 'index.html');
	res.writeHead(200, { 'content-type': types[extname(file)] ?? 'application/octet-stream' });
	res.end(readFileSync(file));
});

// The event socket: handshake, then the hello frame; the app shows itself live.
http.on('upgrade', (req, socket) => {
	if (!req.url.startsWith('/v1/events')) return socket.destroy();
	const accept = createHash('sha1').update(req.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
	socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
	const payload = Buffer.from(JSON.stringify({ v: 1, type: 'hello', ping_s: 30 }));
	socket.write(Buffer.concat([Buffer.from([0x81, payload.length]), payload]));
	socket.on('error', () => {});
});

http.listen(port, '127.0.0.1', () => console.log(`mock on http://127.0.0.1:${port} serving ${root}`));
