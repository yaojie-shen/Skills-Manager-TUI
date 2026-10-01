import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { normalizeBase } from '../src/lib/base.mjs';

test('base path defaults to root and normalizes repository paths', () => {
  for (const input of [undefined, '', '/', '  / ']) assert.equal(normalizeBase(input), '/');
  for (const input of ['Skills-Manager-TUI', '/Skills-Manager-TUI', '/Skills-Manager-TUI/', '//Skills-Manager-TUI//']) {
    assert.equal(normalizeBase(input), '/Skills-Manager-TUI/');
  }
  assert.equal(normalizeBase('/nested/docs/'), '/nested/docs/');
});

test('invalid base paths fail clearly rather than producing broken links', () => {
  for (const input of ['https://example.com', '/a?b', '/a#b', '/..', '/a/./b', '/a\\b', '/%2e%2e']) {
    assert.throws(() => normalizeBase(input), /SITE_BASE/);
  }
});

test('landing installation command stays identical to the root README', () => {
  const readme = readFileSync(new URL('../../README.md', import.meta.url), 'utf8');
  const content = readFileSync(new URL('../src/lib/content.ts', import.meta.url), 'utf8');
  const command = content.match(/export const installCommand = '([^']+)';/)?.[1];
  assert.ok(command, 'shared install command exists');
  assert.ok(readme.includes(command), 'README and landing use the same command');
});

test('both locale landings are wired to the same component', () => {
  for (const [file, locale] of [['index.astro', 'en'], ['zh-cn/index.astro', 'zh-cn']]) {
    const page = readFileSync(new URL(`../src/pages/${file}`, import.meta.url), 'utf8');
    assert.ok(page.includes(`<Landing locale="${locale}" />`));
  }
});

test('link validator catches missing targets and respects a repository base', async () => {
  const { mkdtempSync, mkdirSync, writeFileSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { spawnSync } = await import('node:child_process');
  const { fileURLToPath } = await import('node:url');
  const root = mkdtempSync(join(tmpdir(), 'skills-site-links-'));
  try {
    mkdirSync(join(root, 'guide'));
    writeFileSync(join(root, 'index.html'), '<a href="/repo/guide/#start">Guide</a>');
    writeFileSync(join(root, 'guide/index.html'), '<h1 id="start">Start</h1><a href="../">Home</a>');
    writeFileSync(join(root, '404.html'), '<a href="/repo/missing-404/">Expected fallback link</a>');
    const run = () => spawnSync(process.execPath, [fileURLToPath(new URL('./check-links.mjs', import.meta.url)), root], { encoding: 'utf8', env: { ...process.env, SITE_BASE: '/repo/' } });
    let result = run();
    assert.equal(result.status, 0, result.stderr);
    writeFileSync(join(root, 'index.html'), '<a href="/guide/">Escaped base</a><a href="/repo/missing/">Missing</a><a href="/repo/guide/#missing">Fragment</a>');
    result = run();
    assert.equal(result.status, 1);
    assert.match(result.stderr, /escapes configured base/);
    assert.match(result.stderr, /missing target/);
    assert.match(result.stderr, /missing fragment/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
