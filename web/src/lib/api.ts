/** The board over `/v1/rpc` as the signed-in owner: session cookie plus the CSRF header. */

import type { Session } from './types';

export class RpcError extends Error {
	code: string;
	constructor(code: string, message: string) {
		super(message);
		this.code = code;
	}
}

let session: Session | null = null;
const sessionListeners = new Set<(s: Session) => void>();

/** Calls `fn` with every session loaded, the reload after a stale token included. */
export function onSession(fn: (s: Session) => void): () => void {
	sessionListeners.add(fn);
	return () => sessionListeners.delete(fn);
}

let signingIn = false;

/** Sends the browser to sign in, then back to the current view; once per page load, so the
 * concurrent 401s of a resumed tab start one sign-in. */
export function signIn(): void {
	if (signingIn) return;
	signingIn = true;
	location.href = '/auth/login?next=' + encodeURIComponent('/' + location.hash);
}

export async function loadSession(): Promise<Session> {
	const r = await fetch('/v1/session', { credentials: 'same-origin', cache: 'no-store' });
	if (r.status === 401) {
		signIn();
		throw new RpcError('unauthenticated', 'Signed out');
	}
	if (!r.ok) throw new RpcError('internal', `The server answered ${r.status}`);
	const loaded = (await r.json()) as Session;
	session = loaded;
	for (const fn of sessionListeners) fn(loaded);
	return loaded;
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

/** One read (the web app writes nothing); retried after a dropped connection and once after
 * a session reload. */
export async function rpc<T>(op: string, body: object = {}): Promise<T> {
	const request: Record<string, unknown> = {
		v: 1,
		type: 'request',
		request_id: crypto.randomUUID(),
		op,
		body
	};
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
