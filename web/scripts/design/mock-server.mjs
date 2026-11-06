// Design-only mock of the conde web API over the design fixture, serving a built app.
// Usage: node scripts/design/mock-server.mjs <build-dir> <port>
// Answers /v1/session, /v1/rpc (owner.list, owner.history, owner.message, owner.feed, owner.thread,
// owner.search, fleet.list, channel.list, machine.status and machine.setup (no machine until
// setup, then one being set up), the sign-in relay's three ops (helpers that answer at once:
// Claude's link, then done on a well-formed code; GitHub's and Codex's device code, which, as the
// real helpers print `url` then `waiting`, reads as waiting with its code); the feed, thread and search ops per docs/web-ui-v2/ia.md
// "Server contract"; owner.notify, the one write, changes the fixture's settings for the server's
// life and, like the board, wants an idempotency key and one of the owner's agents),
// /v1/events (a WebSocket that sends the hello frame), /keys (the server's layout, masthead and
// keys templates from crates/conde-remote/src/web/pages.rs, filled with the fixture) and
// /join/<anything> (the join page's template from crates/conde-remote/src/web/join.rs in the
// same layout, signed out). POST /mock/machine-joined makes the machine connected (it joined).

import { createServer } from 'node:http';
import { createHash } from 'node:crypto';
import { readFileSync, existsSync, statSync } from 'node:fs';
import { join, extname, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import * as fx from '../../src/lib/design/fixture.ts';
import { navAgents, navChannels } from '../../src/lib/nav.svelte.ts';

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

/** The owner's machine: none until `machine.setup`. */
let machine = null;

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
		case 'owner.notify': {
			const agent = b.agent?.includes('/') ? b.agent : `${fx.OWNER}/${b.agent}`;
			const refuse = (code, message) => json(res, 200, { ok: false, error: { code, message } });
			if (!r.idempotency_key) return refuse('bad_request', "'owner.notify' needs an idempotency_key (1-200 chars)");
			if (!fx.ownAgents.includes(agent)) return refuse('forbidden', `'${b.agent}' is not one of your agents`);
			if (b.notify !== 'mentions' && b.notify !== 'all') return refuse('bad_request', "notify is 'mentions' or 'all'");
			if (!fx.channels[b.channel]?.members.includes(agent)) return refuse('forbidden', `${agent} is not a member of ${b.channel}`);
			fx.notify[`${agent} ${b.channel}`] = b.notify;
			result = { agent, channel: b.channel, notify: b.notify };
			break;
		}
		case 'machine.status':
			if (!machine) return json(res, 200, { ok: false, error: { code: 'not_found', message: 'no machine is set up for this person' } });
			// relay-github and relay-codex print `waiting` right after their code: the store has passed `url`
			if (machine.relay && machine.relay.flow !== 'claude' && machine.relay.state === 'url') machine.relay = { ...machine.relay, state: 'waiting' };
			result = { ...machine, now: new Date().toISOString() };
			break;
		case 'machine.setup': {
			if (!r.idempotency_key) return json(res, 200, { ok: false, error: { code: 'bad_request', message: "'machine.setup' needs an idempotency_key (1-200 chars)" } });
			const at = new Date().toISOString();
			machine ??= { state: 'requested', host_id: `vm-agent-${fx.OWNER}`, requested_at: at, minted_at: null, deploy_state: 'Running', deploy_error: null, probe_id: null, ready_at: null, session_online_at: null, sign_in: null, relay: null, admins: [] };
			result = { ...machine, now: at, deploy: null };
			break;
		}
		case 'machine.relay.start': {
			const at = new Date().toISOString();
			if (!r.idempotency_key) return json(res, 200, { ok: false, error: { code: 'bad_request', message: "'machine.relay.start' needs an idempotency_key (1-200 chars)" } });
			if (!machine) return json(res, 200, { ok: false, error: { code: 'not_found', message: 'no machine is set up for you' } });
			if (!['claude', 'github', 'codex'].includes(b.flow)) return json(res, 200, { ok: false, error: { code: 'bad_request', message: 'flow is claude, github or codex' } });
			const relay_id = `relay-${Date.now()}`;
			const device = { github: 'https://github.com/login/device', codex: 'https://auth.openai.com/codex/device' };
			machine.relay =
				b.flow === 'claude'
					? { relay_id, flow: 'claude', state: 'url', url: 'https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=https%3A%2F%2Fconsole.anthropic.com%2Foauth%2Fcode%2Fcallback&scope=user%3Ainference&state=journey', user_code: null, expires_at: null, reason: null, at }
					: { relay_id, flow: b.flow, state: 'url', url: device[b.flow], user_code: 'WDJB-MJHT', expires_at: new Date(Date.now() + 15 * 60_000).toISOString(), reason: null, at };
			result = { relay_id };
			break;
		}
		case 'machine.relay.code':
		case 'machine.relay.cancel': {
			const live = machine?.relay && machine.relay.relay_id === b.relay_id && ['starting', 'url', 'waiting'].includes(machine.relay.state);
			const code = r.op === 'machine.relay.code';
			if (code && (typeof b.code !== 'string' || b.code.length < 1 || b.code.length > 256 || /\p{Cc}/u.test(b.code))) {
				return json(res, 200, { ok: false, error: { code: 'bad_request', message: 'the code is 1 to 256 characters without control characters' } });
			}
			if (!live || (code && (machine.relay.flow !== 'claude' || machine.relay.state !== 'url'))) {
				return json(res, 200, { ok: false, error: { code: 'conflict', message: code ? 'this sign-in is not waiting for a code' : 'this sign-in is not running' } });
			}
			machine.relay = { ...machine.relay, state: code ? 'done' : 'cancelled', at: new Date().toISOString() };
			result = { relay_id: b.relay_id };
			break;
		}
		default:
			return json(res, 200, { ok: false, error: { code: 'forbidden', message: `op ${r.op} not mocked` } });
	}
	json(res, 200, { v: 1, type: 'response', request_id: r.request_id, ok: true, result });
}

// The server's pages (pages.rs): its layout, user menu and keys templates, stylesheet and
// scripts, rendered by a small subset of askama (what those templates use) over fixture data.
const pagesRs = readFileSync(join(here, '../../../crates/conde-remote/src/web/pages.rs'), 'utf8');
const rustStr = (name) => pagesRs.match(new RegExp(`const ${name}: &str = r#"([\\s\\S]*?)"#;`))[1];
const template = (start) => pagesRs.match(new RegExp(`source = r#"(${start}[\\s\\S]*?)"#\\n\\s*\\)\\]`))[1];
const css = rustStr('CSS');
const themeJs = rustStr('THEME_JS');
const menuJs = rustStr('MENU_JS');
const layoutTpl = template('<!doctype html>');
const menuTpl = template('<details class="menu-panel">');
const keysTpl = template('<h1>Host keys</h1>');
const sha = (js) => `'sha256-${createHash('sha256').update(js).digest('base64')}'`;
// The pages' CSP as pages.rs sends it, so the browser checks the scripts' hashes and the fonts.
const pagesCsp = (signedIn) =>
	`default-src 'none'; style-src 'unsafe-inline'; script-src ${sha(themeJs)}${signedIn ? ` ${sha(menuJs)}` : ''}; img-src 'self'; font-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'`;

/** Renders an askama template with `{{ path|filter }}`, `{% if [!]path[.is_empty()] %}`,
 * `{% if let Some(x) = path %}`, `{% else %}` and `{% for x in path %}`; HTML-escaped unless `safe`. */
function askama(src, ctx) {
	const parts = src.split(/(\{\{[\s\S]*?\}\}|\{%[\s\S]*?%\})/);
	let at = 0;
	const lookup = (scope, path) => path.split('.').reduce((v, k) => (v == null ? v : v[k]), scope);
	const value = (scope, expr) => {
		const not = expr.startsWith('!');
		const e = not ? expr.slice(1) : expr;
		const empty = e.endsWith('.is_empty()');
		let v = lookup(scope, empty ? e.slice(0, -'.is_empty()'.length) : e);
		if (empty) v = v.length === 0;
		return not ? !v : v;
	};
	const esc = (v) => String(v).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&#34;', "'": '&#39;' })[c]);
	const enc = (strict) => (v) => {
		const e = encodeURIComponent(String(v)).replace(/[!'()*]/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`);
		return strict ? e : e.replace(/%2F/g, '/');
	};
	const filters = { safe: (v) => String(v), urlencode: enc(false), urlencode_strict: enc(true) };
	// Parses up to one of `stops`; returns the nodes and the stop tag.
	function block(stops) {
		const nodes = [];
		while (at < parts.length) {
			const p = parts[at++];
			if (p.startsWith('{{')) {
				const [expr, filter] = p.slice(2, -2).trim().split('|');
				nodes.push((scope) => (filter ? filters[filter](value(scope, expr)) : esc(value(scope, expr))));
			} else if (p.startsWith('{%')) {
				const tag = p.slice(2, -2).trim();
				if (stops.includes(tag)) return [nodes, tag];
				let m;
				if ((m = tag.match(/^if let Some\((\w+)\) = (.+)$/)) || (m = tag.match(/^if (.+)$/))) {
					const [bind, cond] = m.length === 3 ? [m[1], m[2]] : [null, m[1]];
					const [then, stop] = block(['else', 'endif']);
					const other = stop === 'else' ? block(['endif'])[0] : [];
					nodes.push((scope) => {
						const v = value(scope, cond);
						const yes = bind ? v != null : !!v;
						return run(yes ? then : other, yes && bind ? { ...scope, [bind]: v } : scope);
					});
				} else if ((m = tag.match(/^for (\w+) in (.+)$/))) {
					const [, name, list] = m;
					const [body] = block(['endfor']);
					nodes.push((scope) => value(scope, list).map((x) => run(body, { ...scope, [name]: x })).join(''));
				} else throw new Error(`template tag not mocked: ${tag}`);
			} else nodes.push(() => p);
		}
		return [nodes, null];
	}
	const run = (nodes, scope) => nodes.map((n) => n(scope)).join('');
	return run(block([])[0], ctx);
}

const mark = readFileSync(join(here, '../../static/app/mark.svg'), 'utf8').trim();
const server = 'https://conde.nativeai.space';
const layout = (title, body, shell = null) => askama(layoutTpl, { title, theme: themeJs, css, mark, shell, body, menu_js: menuJs, nonce: null, js: '' });

/** The masthead as the server fills it: the app's own lists over the fixture's owner.list and fleet.list. */
function shell() {
	const rail = fx.rail();
	const short = (n) => (n.startsWith(`${fx.OWNER}/`) ? n.slice(fx.OWNER.length + 1) : n);
	const agents = navAgents(rail, fx.fleet).map((a) => ({ name: short(a.name), state: a.state, held: a.held || null, stuck: a.stuck || null }));
	const channels = navChannels(rail).map((c) => ({ name: c, slug: c.slice(1) }));
	const n = agents.length;
	const scope = `You see what your ${n === 1 ? 'agent' : n > 1 ? `${n} agents` : 'agents'} can read.`;
	const menu = (more) =>
		askama(menuTpl, { display: fx.session.display, user: fx.session.user, csrf: 'mock', admin: (fx.fleet.admins ?? []).includes(fx.session.user), more });
	return { agents, channels, scope, menu: menu(false), more: menu(true), machines: true };
}

function keysPage() {
	const rows = fx.keys.map((k) => ({ ...k, revoked: k.revoked ?? null }));
	const apps = fx.apps
		.map(
			(a) =>
				`<tr>\n<td data-label="App"><code>${a.host}</code> <span class="muted">&#8220;${a.name}&#8221;</span></td><td data-label="Created">${a.created}</td><td data-label="Last used">${a.last_used}</td><td data-label="Expires">${a.expires}</td>\n<td><form class="inline"><button class="danger">Revoke</button></form></td>\n</tr>`
		)
		.join('\n');
	// Connected apps is the design of docs/REMOTE-MCP.md, not in pages.rs yet.
	const body = `${askama(keysTpl, { csrf: 'mock', server, rows })}
<h1>Connected apps</h1>
<p class="muted">Apps such as Claude that act on this board as you, through its MCP endpoint <code>${server}/mcp</code>. Revoking one ends its access at once.</p>
<div class="wrap"><table>
<thead><tr><th>App</th><th>Created</th><th>Last used</th><th>Expires</th><th></th></tr></thead>
<tbody>
${apps}</tbody>
</table></div>`;
	return layout('Host keys', body, shell());
}

// The join page: join.rs's template, its two branches and fields filled for an invited teammate.
const joinRs = readFileSync(join(here, '../../../crates/conde-remote/src/web/join.rs'), 'utf8');
const joinTemplate = joinRs.match(/source = r#"(<div class="join">[\s\S]*?)"#/)[1];
const joinConst = (name) => joinRs.match(new RegExp(`const ${name}: &str = "([^"]*)";`))[1];
function joinPage() {
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
	return layout('Join nativeai', body);
}

const http = createServer((req, res) => {
	const url = new URL(req.url, 'http://x');
	if (url.pathname === '/v1/session') return json(res, 200, { ...fx.session, machines: true });
	if (url.pathname === '/v1/rpc' && req.method === 'POST') return rpc(req, res);
	if (url.pathname === '/mock/machine-joined' && req.method === 'POST') {
		if (machine) machine = { ...machine, state: 'connected', minted_at: new Date().toISOString(), deploy_state: 'Succeeded' };
		return json(res, 200, { ok: Boolean(machine) });
	}
	if (url.pathname === '/keys') {
		res.writeHead(200, { 'content-type': 'text/html; charset=utf-8', 'content-security-policy': pagesCsp(true) });
		return res.end(keysPage());
	}
	if (url.pathname.startsWith('/join/')) {
		res.writeHead(200, { 'content-type': 'text/html; charset=utf-8', 'content-security-policy': pagesCsp(false) });
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
