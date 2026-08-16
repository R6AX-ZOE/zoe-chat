const STORAGE_KEY = 'zoe.theme';
const mql = window.matchMedia('(prefers-color-scheme: dark)');
export function getTheme() {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved === 'light' || saved === 'dark' || saved === 'system')
        return saved;
    return 'system';
}
export function setTheme(theme) {
    localStorage.setItem(STORAGE_KEY, theme);
    applyTheme();
}
export function applyTheme() {
    const theme = getTheme();
    const effective = theme === 'system' ? (mql.matches ? 'dark' : 'light') : theme;
    document.documentElement.dataset.theme = effective;
    document.documentElement.style.colorScheme = effective;
}
mql.addEventListener('change', () => {
    if (getTheme() === 'system')
        applyTheme();
});
