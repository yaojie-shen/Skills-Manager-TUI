import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
const site=fileURLToPath(new URL('../',import.meta.url));
const data=JSON.parse(readFileSync(resolve(site,'src/generated/tui-captures.json'),'utf8'));
const component=readFileSync(resolve(site,'src/components/CapturedTui.astro'),'utf8');
const css=readFileSync(resolve(site,'src/styles/captured-tui.css'),'utf8');
const names=['library','tags','presets','agents','repos','health'];
const landmarks={library:['search skills','preview'],tags:['Filter tags','Tag: engineering'],presets:['Filter presets','Preset: Backend Launch'],agents:['Agents','Scope'],repos:['Filter sources','source details','Source skills'],health:['Filter health entries','faults','detail']};
test('six captures are real, styled, rectangular and privacy-safe',()=>{
 assert.equal(data.schemaVersion,1);assert.equal(data.provenance.renderer,'ratatui::backend::TestBackend');assert.equal(data.provenance.component,'tui::app::App');assert.match(data.provenance.interaction,/Msg::Key/);assert.deepEqual(data.order,names);assert.equal(data.width,112);assert.equal(data.height,32);
 const all=JSON.stringify(data);assert.doesNotMatch(all,/\/Users\/|\/private\/|\/tmp\/|bytedance|Counting…|⠋/);assert.match(all,/example\.invalid/);
 for(const name of names){const f=data.frames[name];assert.equal(f.rows.length,32);for(const row of f.rows)assert.equal(row.reduce((n,r)=>n+r.cells,0),112,`${name} row width`);const text=f.rows.flatMap(r=>r).map(r=>r.text).join('');for(const mark of landmarks[name])assert.ok(text.includes(mark),`${name}: ${mark}`);assert.ok(f.rows.flatMap(r=>r).some(r=>r.fg!=='reset'||r.bg!=='reset'||r.mods.length));assert.match(f.hash,/^sha256:[a-f0-9]{64}$/);}
});
test('viewer is a six-frame pixel proof sheet without presentation overlays',()=>{
 assert.match(component,/tui-captures\.json/);assert.match(component,/data-selected-frame/);assert.match(component,/<figure class="capture-frame"/);assert.match(component,/<figcaption class="capture-caption"/);assert.match(component,/data-proof-grid/);assert.match(component,/data-proof=/);assert.match(component,/ArrowLeft/);assert.match(component,/ArrowRight/);assert.match(component,/aria-current/);assert.match(component,/aria-live="polite" aria-atomic="true"/);assert.match(component,/data-hotspots/);assert.match(component,/inert=/);assert.match(component,/aria-hidden="true"/);assert.match(component,/is-entering/);
 assert.doesNotMatch(component,/capture-terminal-scroller" tabindex|capture-focus|--focus-|setInterval|setTimeout|visibilitychange/);
 assert.doesNotMatch(component,/role="tab"|role="tabpanel"|data-frame-count|data-progress|data-search|data-agent|data-reset|demoSkills|annotation-rail|capture-targets|capture-leaders|data-mobile-tab|capture-mobile-notes|drawLeader|ResizeObserver/);
 assert.match(css,/@font-face[^}]*JB Nerd Mono/s);assert.match(css,/\.capture-frame\.is-entering\{[^}]*animation:capture-enter 140ms steps\(3,end\)/);assert.match(css,/prefers-reduced-motion[^}]*\}[^{]*\.capture-frame\.is-entering\{animation:none/);assert.match(css,/\.capture-terminal-scroller\{[^}]*overflow:hidden/);assert.match(css,/\.capture-pre\{[^}]*width:max-content[^}]*white-space:pre/);assert.match(css,/font:400 clamp\([^}]*'JB Nerd Mono'/);assert.match(css,/\.capture-proof\{[^}]*grid-template-columns:repeat\(3,1fr\)/);assert.match(css,/@media\(max-width:800px\)/);
 assert.doesNotMatch(css,/capture-focus|--focus-|9999px|filter:|annotation-rail|capture-targets|capture-leader|capture-mobile-tabs|capture-mobile-notes|box-shadow/);
 assert.match(css,/\.proof-preview pre\{[^}]*font:400 clamp\(/);
 assert.doesNotMatch(css,/\.capture-pre\{[^}]*transform:scale|\.proof-preview pre\{[^}]*transform:scale/);
});
