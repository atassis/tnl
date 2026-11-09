import {useEffect, useLayoutEffect, useState} from 'react';
import {parseTheme, resolveTheme, themeFragment} from './theme';
import type {ThemeMode} from './theme';

export function useTheme() {
  const [mode, setModeState] = useState<ThemeMode>(() => parseTheme(new URLSearchParams(location.hash.slice(1)).get('theme')));
  const [systemDark, setSystemDark] = useState(() => matchMedia('(prefers-color-scheme: dark)').matches);
  useEffect(() => {
    const media = matchMedia('(prefers-color-scheme: dark)');
    const update = () => setSystemDark(media.matches);
    media.addEventListener('change', update);
    return () => media.removeEventListener('change', update);
  }, []);
  const resolved = resolveTheme(mode, systemDark);
  useLayoutEffect(() => { document.documentElement.dataset.theme = resolved; }, [resolved]);
  const setMode = (next: ThemeMode) => {
    history.replaceState(history.state, '', location.pathname + location.search + themeFragment(location.hash, next));
    setModeState(next);
  };
  return {mode, resolved, setMode};
}
