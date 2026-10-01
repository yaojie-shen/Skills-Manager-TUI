import assert from 'node:assert/strict';
import test from 'node:test';
import { existsSync, readFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const site = fileURLToPath(new URL('../', import.meta.url));
const repo = resolve(site, '..');
const topics = ['index', 'installation', 'quickstart', 'library', 'deployment', 'tags-presets', 'updates', 'health', 'sync', 'configuration', 'reference', 'troubleshooting'];
const locales = ['', 'zh-cn/'];

for (const locale of locales) {
  test(`${locale || 'English/'} handbook contains all twelve substantive pages`, () => {
    for (const topic of topics) {
      const markdown = resolve(site, 'src/content/docs', locale, 'guide', `${topic}.md`);
      const mdx = resolve(site, 'src/content/docs', locale, 'guide', `${topic}.mdx`);
      const file = existsSync(markdown) ? markdown : mdx;
      assert.ok(existsSync(file), `Missing ${markdown} or ${mdx}`);
      const text = readFileSync(file, 'utf8');
      assert.match(text, /^---\r?\n[\s\S]*?\btitle:/, `${file}: title`);
      assert.match(text, /^---\r?\n[\s\S]*?\bdescription:/, `${file}: description`);
      assert.ok(text.length > 500, `${file}: must contain a useful guide, not a stub`);
      assert.doesNotMatch(text, /\bTODO\b|coming soon|即将推出/, `${file}: unfinished placeholder`);
    }
  });
}

test('README local links resolve to actual repository files', () => {
  const name = 'README.md';
  const file = resolve(repo, name);
  const text = readFileSync(file, 'utf8');
  for (const match of text.matchAll(/\]\(([^\s)]+)\)/g)) {
    const target = match[1].split('#')[0];
    if (!target || /^(?:https?:|mailto:)/.test(target)) continue;
    assert.ok(existsSync(resolve(dirname(file), target)), `${name}: missing ${target}`);
  }
});

test('README contains the installer and Library setup', () => {
  const command = 'curl -fsSL https://github.com/yaojie-shen/Skills-Manager-TUI/raw/main/install.sh | sh';
  const text = readFileSync(resolve(repo, 'README.md'), 'utf8');
  assert.ok(text.includes(command), 'README.md: install command');
  assert.ok(text.includes('export SKILLS_HOME="$HOME/.skills"'), 'README.md: library setup');
});

test('homepage centers real captured tabs rather than a web simulator', () => {
  const landing = readFileSync(resolve(site, 'src/components/Landing.astro'), 'utf8');
  const viewer = readFileSync(resolve(site, 'src/components/CapturedTui.astro'), 'utf8');
  assert.match(landing, /<CapturedTui locale=/);
  assert.doesNotMatch(landing + viewer, /data-search|data-agent|data-reset|demoSkills/);
  assert.match(viewer, /data-selected-frame/);
  assert.match(viewer, /data-proof-grid/);
  assert.doesNotMatch(viewer, /capture-focus|--focus-/);
  assert.match(viewer, /capture-caption/);
  const heroCapture = readFileSync(resolve(site, 'src/components/HeroCapture.astro'), 'utf8');
  assert.match(heroCapture, /hero-library-card\.json/);
  assert.doesNotMatch(heroCapture, /data-hero-tab|class="pipeline"/);
  assert.match(landing, /class="cli-section/);
  const hero = landing.indexOf('class="hero ');
  const install = landing.indexOf('class="install-section');
  const showcase = landing.indexOf('<CapturedTui locale=');
  const cli = landing.indexOf('class="cli-section');
  assert.ok(hero < install && install < showcase && showcase < cli, 'homepage order must be hero, installation, TUI proof sheet, then Agent CLI');
  assert.doesNotMatch(landing, /class="preflight|class="system-map|class="library-map/);
  assert.doesNotMatch(viewer, /role="tab"|role="tabpanel"|data-frame-count|data-progress|annotation-rail|capture-targets|capture-leaders|data-mobile-tab|capture-mobile-notes/);
});
