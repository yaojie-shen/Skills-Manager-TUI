import assert from 'node:assert/strict';
import test from 'node:test';
import { docsLanguageHref } from '../src/lib/docs-language-url.mjs';

test('docs language URLs handle bases, locales, routes, and browser URL state', () => {
  const cases = [
    {
      name: 'root English route to Chinese keeps query and drops fragment',
      input: { pathname: '/guide/setup/', base: '/', currentLocale: 'en', targetLocale: 'zh-cn', search: '?tab=cli', hash: '#install' },
      expected: '/zh-cn/guide/setup/?tab=cli',
    },
    {
      name: 'root Chinese route to English removes only the leading locale',
      input: { pathname: '/zh-cn/guide/zh-cn/topic', base: '/', currentLocale: 'zh-cn', targetLocale: 'en', search: '?q=1', hash: '#part' },
      expected: '/guide/zh-cn/topic?q=1',
    },
    {
      name: 'repository English route to Chinese preserves no trailing slash',
      input: { pathname: '/Skills-Manager-TUI/reference/api', base: '/Skills-Manager-TUI/', currentLocale: 'en', targetLocale: 'zh-cn' },
      expected: '/Skills-Manager-TUI/zh-cn/reference/api',
    },
    {
      name: 'repository Chinese route to English preserves trailing slash',
      input: { pathname: '/Skills-Manager-TUI/zh-cn/reference/api/', base: '/Skills-Manager-TUI/', currentLocale: 'zh-cn', targetLocale: 'en' },
      expected: '/Skills-Manager-TUI/reference/api/',
    },
    {
      name: 'same language retains query and fragment',
      input: { pathname: '/Skills-Manager-TUI/zh-cn/', base: '/Skills-Manager-TUI/', currentLocale: 'zh-cn', targetLocale: 'zh-cn', search: '?view=all', hash: '#top' },
      expected: '/Skills-Manager-TUI/zh-cn/?view=all#top',
    },
    {
      name: 'root pages switch to the localized root',
      input: { pathname: '/', base: '/', currentLocale: 'en', targetLocale: 'zh-cn' },
      expected: '/zh-cn/',
    },
    {
      name: 'base is not stripped when the pathname does not start with it',
      input: { pathname: '/guide/', base: '/Skills-Manager-TUI/', currentLocale: 'en', targetLocale: 'zh-cn' },
      expected: '/Skills-Manager-TUI/zh-cn/guide/',
    },
  ];

  for (const { name, input, expected } of cases) {
    assert.equal(docsLanguageHref(input), expected, name);
  }
});
