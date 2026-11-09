export type ThemeMode = 'auto' | 'light' | 'dark';
export function resolveTheme(mode: ThemeMode, systemDark: boolean): 'light' | 'dark' {
  return mode === 'auto' ? (systemDark ? 'dark' : 'light') : mode;
}
export function parseTheme(value: string | null): ThemeMode {
  return value === 'light' || value === 'dark' ? value : 'auto';
}
export function themeFragment(fragment: string, mode: ThemeMode): string {
  const params = new URLSearchParams(fragment.startsWith('#') ? fragment.slice(1) : fragment);
  if (mode === 'auto') params.delete('theme'); else params.set('theme', mode);
  const value = params.toString();
  return value ? `#${value}` : '';
}
