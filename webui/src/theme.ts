// 深色/浅色主题:data-theme 驱动 CSS 变量。
// 默认跟随系统(prefers-color-scheme),用户选择持久化。

export type Theme = 'light' | 'dark' | 'system';

const STORAGE_KEY = 'zoe.theme';

const mql = window.matchMedia('(prefers-color-scheme: dark)');

export function getTheme(): Theme {
  const saved = localStorage.getItem(STORAGE_KEY);
  if (saved === 'light' || saved === 'dark' || saved === 'system') return saved;
  return 'system';
}

export function setTheme(theme: Theme): void {
  localStorage.setItem(STORAGE_KEY, theme);
  applyTheme();
}

export function applyTheme(): void {
  const theme = getTheme();
  const effective = theme === 'system' ? (mql.matches ? 'dark' : 'light') : theme;
  document.documentElement.dataset.theme = effective;
  document.documentElement.style.colorScheme = effective;
}

// 系统主题变化时即时跟随(system 模式下)
mql.addEventListener('change', () => {
  if (getTheme() === 'system') applyTheme();
});
