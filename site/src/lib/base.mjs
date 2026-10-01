/** A path only, not an origin. Shared by Astro config and build tests. */
export function normalizeBase(value = '/') {
  const path = value.trim();
  if (!path || path === '/') return '/';
  if (path.includes('://') || /[?#\\]/.test(path)) {
    throw new Error('SITE_BASE must be a URL path, such as /Skills-Manager-TUI/.');
  }
  const segments = path.split('/').filter(Boolean);
  if (segments.some((segment) => segment === '.' || segment === '..' || !/^[\w-]+$/.test(segment))) {
    throw new Error('SITE_BASE may contain only letters, numbers, hyphens, underscores and slashes.');
  }
  return `/${segments.join('/')}/`;
}
