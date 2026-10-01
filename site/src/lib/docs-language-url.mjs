const CHINESE_LOCALE = 'zh-cn';

/** Build a docs locale URL without treating locale-like nested segments as prefixes. */
export function docsLanguageHref({
  pathname,
  base,
  targetLocale,
  currentLocale,
  search = '',
  hash = '',
}) {
  if (targetLocale === currentLocale) return `${pathname}${search}${hash}`;

  const relative = (pathname.startsWith(base)
    ? pathname.slice(base.length)
    : pathname.replace(/^\/+/, ''))
    .replace(/^zh-cn(?:\/|$)/, '');
  const localizedPath = targetLocale === CHINESE_LOCALE
    ? `${base}${CHINESE_LOCALE}/${relative}`
    : `${base}${relative}`;

  return `${localizedPath}${search}`;
}
