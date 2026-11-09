import { expect, it } from 'vitest';
import { resolveTheme } from './theme';
it('resolves Auto and preserves explicit overrides', () => {
  expect(resolveTheme('auto', true)).toBe('dark');
  expect(resolveTheme('auto', false)).toBe('light');
  expect(resolveTheme('dark', false)).toBe('dark');
  expect(resolveTheme('light', true)).toBe('light');
});

import {parseTheme,themeFragment} from './theme';
it('allowlists fragment mode and preserves capability and unknown fields',()=>{
  expect(parseTheme('dark')).toBe('dark');expect(parseTheme('light')).toBe('light');
  expect(parseTheme('url(https://invalid)')).toBe('auto');expect(parseTheme(null)).toBe('auto');
  const dark=themeFragment('#token=private-capability&other=keep','dark');
  const params=new URLSearchParams(dark.slice(1));
  expect(params.get('theme')).toBe('dark');expect(params.get('token')).toBe('private-capability');expect(params.get('other')).toBe('keep');
  expect(new URLSearchParams(themeFragment(dark,'auto').slice(1)).has('theme')).toBe(false);
});
