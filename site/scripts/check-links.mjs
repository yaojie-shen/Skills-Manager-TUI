/** Validate local href/src targets and fragment IDs in a built static site.
 * Usage: node scripts/check-links.mjs [dist-directory]
 * SITE_BASE must match the value used for `npm run build`.
 */
import { readdirSync, readFileSync, existsSync, statSync } from 'node:fs';
import { resolve, join, relative } from 'node:path';
import { normalizeBase } from '../src/lib/base.mjs';

const root = resolve(process.argv[2] || 'dist');
const base = normalizeBase(process.env.SITE_BASE);
const origin = 'https://local.invalid';
function walk(path) {
  return readdirSync(path, { withFileTypes: true }).flatMap((entry) =>
    entry.isDirectory() ? walk(join(path, entry.name)) : [join(path, entry.name)]);
}
if (!existsSync(root)) throw new Error('Build the site before checking links.');
const htmlFiles = walk(root).filter((file) => file.endsWith('.html'));
if (!htmlFiles.length) throw new Error('No HTML pages found. Build the site before checking links.');
const ids = new Map();
const errors = [];
const decode = (value) => value.replaceAll('&amp;', '&').replaceAll('&#39;', "'").replaceAll('&quot;', '"');
for (const file of htmlFiles) {
  const html = readFileSync(file, 'utf8');
  ids.set(file, new Set([...html.matchAll(/\bid\s*=\s*["']([^"']+)["']/g)].map((match) => decode(match[1]))));
}
let count = 0;
for (const file of htmlFiles) {
  const path = relative(root, file).replaceAll('\\', '/').replace(/index\.html$/, '');
  if (path === '404.html') continue;
  const current = `${origin}${base}${path}`;
  const html = readFileSync(file, 'utf8');
  for (const match of html.matchAll(/\b(?:href|src)\s*=\s*["']([^"']*)["']/g)) {
    const href = decode(match[1]);
    if (!href || /^(?:mailto:|tel:|data:|javascript:)/i.test(href)) continue;
    const url = new URL(href, current);
    if (url.origin !== origin) continue;
    count++;
    if (!url.pathname.startsWith(base)) {
      errors.push(`${path}: ${href} escapes configured base ${base}`);
      continue;
    }
    let target = resolve(root, decodeURIComponent(url.pathname.slice(base.length)) || '.');
    if (!existsSync(target)) {
      errors.push(`${path}: missing target ${href}`);
      continue;
    }
    if (statSync(target).isDirectory()) target = join(target, 'index.html');
    if (!existsSync(target)) {
      errors.push(`${path}: no index page for ${href}`);
      continue;
    }
    if (url.hash && ids.has(target) && !ids.get(target).has(decodeURIComponent(url.hash.slice(1)))) {
      errors.push(`${path}: missing fragment ${href}`);
    }
  }
}
if (errors.length) {
  console.error(errors.join('\n'));
  process.exitCode = 1;
} else {
  console.log(`Checked ${count} local links/assets across ${htmlFiles.length} HTML pages (base: ${base}).`);
}
