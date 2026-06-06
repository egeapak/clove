import { browser } from '$app/environment';

export interface ThemeDef {
  key: string;
  name: string;
  swatch: [string, string, string]; // accent, surface, text
}

export const THEMES: ThemeDef[] = [
  { key: 'midnight-ide', name: 'Midnight IDE', swatch: ['#58a6ff', '#11161f', '#e6edf3'] },
  { key: 'linear-light', name: 'Linear Light', swatch: ['#5b5bd6', '#ffffff', '#1c1e26'] },
  { key: 'solarized-duo', name: 'Solarized Duo', swatch: ['#268bd2', '#fdf6e3', '#586e75'] },
  { key: 'vibrant-glass', name: 'Vibrant Glass', swatch: ['#b06bff', '#160d33', '#f4f1ff'] }
];

const STORAGE_KEY = 'clove-theme';

class ThemeStore {
  current = $state<string>('midnight-ide');

  init() {
    if (!browser) return;
    const saved = localStorage.getItem(STORAGE_KEY);
    // Default is always midnight-ide; only a valid stored preference overrides it.
    this.current = saved && THEMES.some((t) => t.key === saved) ? saved : 'midnight-ide';
    this.apply();
  }

  set(key: string) {
    this.current = key;
    if (browser) {
      localStorage.setItem(STORAGE_KEY, key);
      this.apply();
    }
  }

  private apply() {
    document.documentElement.dataset.theme = this.current;
  }
}

export const theme = new ThemeStore();
