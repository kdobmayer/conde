// The user menu's side of the theme (theme.ts): the current choice, and changing it.
import { THEME_KEY, asChoice, resolve, type Choice } from './theme';

function stored(): Choice {
	try {
		return asChoice(localStorage.getItem(THEME_KEY));
	} catch {
		return 'system';
	}
}

export const theme = $state({ choice: stored() });

/** Stores the choice (removed for system) and applies it at once. */
export function choose(choice: Choice): void {
	theme.choice = choice;
	try {
		if (choice === 'system') localStorage.removeItem(THEME_KEY);
		else localStorage.setItem(THEME_KEY, choice);
	} catch {
		// applied to this page only
	}
	document.documentElement.dataset.theme = resolve(choice, matchMedia('(prefers-color-scheme: light)').matches);
}

// Another tab's choice: the root follows through the inline script, the menu here.
if (typeof window !== 'undefined')
	addEventListener('storage', (e) => {
		if (e.key === THEME_KEY || e.key === null) theme.choice = stored();
	});
