/**
 * The width of the agent page's conversation list, a per-browser choice made with the drag
 * handle beside it and stored under `conde.agentlist.width`. Nothing stored means the layout's
 * default (290 px, 250 px below 1280 px).
 */

export const WIDTH_KEY = 'conde.agentlist.width';
export const MIN_WIDTH = 200;
export const MAX_WIDTH = 480;

export const clampWidth = (w: number): number => Math.round(Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, w)));

/** A stored value as a width in range, or null (the default) when it is missing or not a number. */
export function asWidth(v: string | null | undefined): number | null {
	if (!v) return null;
	const n = Number(v);
	return Number.isFinite(n) ? clampWidth(n) : null;
}

export function readWidth(storage: Pick<Storage, 'getItem'> | undefined = globalThis.localStorage): number | null {
	try {
		return asWidth(storage?.getItem(WIDTH_KEY));
	} catch {
		return null;
	}
}

/** Stores a width, or forgets it (null) so the default applies again. */
export function writeWidth(w: number | null, storage: Pick<Storage, 'setItem' | 'removeItem'> | undefined = globalThis.localStorage): void {
	try {
		if (w === null) storage?.removeItem(WIDTH_KEY);
		else storage?.setItem(WIDTH_KEY, String(clampWidth(w)));
	} catch {
		// storage blocked: the width holds for this page only
	}
}
