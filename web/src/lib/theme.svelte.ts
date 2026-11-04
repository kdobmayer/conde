// The user menu's side of the theme and the look (theme.ts): the current choices, and changing them.
import { THEME_KEY, UI_KEY, asChoice, asUi, resolve, type Choice, type Ui } from './theme';

function stored(): Choice {
	try {
		return asChoice(localStorage.getItem(THEME_KEY));
	} catch {
		return 'system';
	}
}

function storedUi(): Ui {
	try {
		return asUi(localStorage.getItem(UI_KEY));
	} catch {
		return 'current';
	}
}

export const theme = $state({ choice: stored(), ui: storedUi() });

/** Stores the choice (removed for system) and applies it at once. */
export function choose(choice: Choice): void {
	theme.choice = choice;
	try {
		if (choice === 'system') localStorage.removeItem(THEME_KEY);
		else localStorage.setItem(THEME_KEY, choice);
	} catch {
		// applied to this page only
	}
	apply();
}

/** Stores the look (removed for current) and applies it at once. */
export function chooseUi(ui: Ui): void {
	theme.ui = ui;
	try {
		if (ui === 'current') localStorage.removeItem(UI_KEY);
		else localStorage.setItem(UI_KEY, ui);
	} catch {
		// applied to this page only
	}
	apply();
}

const LIGHT = '(prefers-color-scheme: light)';

/** Puts the choices on the root element. */
function apply(): void {
	const root = document.documentElement;
	root.dataset.theme = resolve(theme.choice, matchMedia(LIGHT).matches);
	if (theme.ui === 'beta') root.dataset.ui = theme.ui;
	else delete root.dataset.ui;
}

// The inline script reapplies storage on both events; the root and the menu follow one choice.
if (typeof window !== 'undefined') {
	addEventListener('storage', () => {
		theme.choice = stored();
		theme.ui = storedUi();
		apply();
	});
	matchMedia(LIGHT).addEventListener('change', apply);
}
