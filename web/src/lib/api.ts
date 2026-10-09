/** The board over `/v1/rpc` as the signed-in human: session cookie plus the CSRF header. */

import type { Session } from './types';

export class RpcError extends Error {
	code: string;
	constructor(code: string, message: string) {
		super(message);
		this.code = code;
	}
}

/** Ops that change the board: each carries an idempotency key, reused on a network retry. */
const MUTATIONS = new Set([
	'message.send',
	'message.read',
	'reaction.set',
	'channel.create',
	'channel.join',
	'channel.leave',
	'channel.topic',
	'channel.invite',
	'profile.set',
	'conversation.read'
]);

let session: Session | null = null;

/** Sends the browser to sign in, then back to the current view. */
export function signIn(): void {
	location.href = '/auth/login?next=' + encodeURIComponent('/' + location.hash);
}

export async function loadSession(): Promise<Session> {
	const r = await fetch('/v1/session', { credentials: 'same-origin', cache: 'no-store' });
	if (r.status === 401) {
		signIn();
		throw new RpcError('unauthenticated', 'Signed out');
	}
	if (!r.ok) throw new RpcError('internal', `The server answered ${r.status}`);
	session = (await r.json()) as Session;
	return session;
}

export function currentSession(): Session | null {
	return session;
}

async function post(request: object, s: Session): Promise<Response> {
	return fetch('/v1/rpc', {
		method: 'POST',
		credentials: 'same-origin',
		headers: { 'content-type': 'application/json', [s.csrf_header]: s.csrf },
		body: JSON.stringify(request)
	});
}

/**
 * One request. A mutation carries one idempotency key for its whole life: resent after a
 * dropped connection and after a session reload, the board commits it once.
 */
export async function rpc<T>(op: string, body: object = {}): Promise<T> {
	const request: Record<string, unknown> = {
		v: 1,
		type: 'request',
		request_id: crypto.randomUUID(),
		op,
		body
	};
	if (MUTATIONS.has(op)) request.idempotency_key = crypto.randomUUID();
	try {
		return await call<T>(request);
	} catch (e) {
		// Another sign-in replaced the session (new CSRF token): reload it and retry once.
		if (!(e instanceof RpcError) || e.code !== 'stale_session') throw e;
		await loadSession();
		return call<T>(request);
	}
}

async function call<T>(request: Record<string, unknown>): Promise<T> {
	const s = session ?? (await loadSession());
	let response: Response | null = null;
	for (let attempt = 0; attempt < 3 && !response; attempt++) {
		try {
			response = await post(request, s);
		} catch (e) {
			if (attempt === 2) throw new RpcError('network', 'The server could not be reached');
			await new Promise((r) => setTimeout(r, 500 * (attempt + 1)));
		}
	}
	const r = response as Response;
	if (r.status === 401) {
		signIn();
		throw new RpcError('unauthenticated', 'Signed out');
	}
	const json = (await r.json().catch(() => null)) as {
		ok?: boolean;
		result?: T;
		error?: { code: string; message: string };
	} | null;
	if (!json || !json.ok) {
		throw new RpcError(
			json?.error?.code ?? 'internal',
			json?.error?.message ?? `The server answered ${r.status}`
		);
	}
	return json.result as T;
}

/** A file as the base64 the board stores. */
export function fileToBase64(file: File): Promise<string> {
	return new Promise((resolve, reject) => {
		const reader = new FileReader();
		reader.onerror = () => reject(reader.error);
		reader.onload = () => {
			const url = String(reader.result);
			resolve(url.slice(url.indexOf(',') + 1));
		};
		reader.readAsDataURL(file);
	});
}
