# Skills Manager site

This Astro + Starlight static site contains the English and Simplified Chinese landing pages and product documentation. It builds independently from the Rust application.

## Architecture

| Path | Responsibility |
| --- | --- |
| `astro.config.mjs` | Starlight integration, locales, sidebar, styles, trailing-slash routes, configurable base |
| `src/content.config.ts` | Starlight `docsLoader` and `docsSchema` collection |
| `src/pages/index.astro` | English landing entry |
| `src/pages/zh-cn/index.astro` | Chinese landing entry |
| `src/components/Landing.astro` | Shared landing layout and TUI capture embedding |
| `src/generated/tui-captures.json` | Terminal capture data used by the landing page and documentation |
| `src/components/CapturedTui.astro` | Selectable TUI viewer with tab hotspots and localized captions |
| `src/components/HeroCapture.astro` | Library frame shown in the hero |
| `scripts/fetch-fonts.mjs` | Downloads pinned, checksummed Silkscreen and JetBrains Mono Nerd Font files for local dev and builds |
| `public/fonts/` | Font provenance and OFL licenses; generated `.ttf` files are ignored |
| `src/components/DocsSiteTitle.astro` | Documentation title/navigation integration |
| `src/lib/content.ts` | Bilingual landing copy and shared installer command |
| `src/lib/base.mjs` | Shared `SITE_BASE` normalization used by config and tests |
| `src/content/docs/guide/` | Twelve English Markdown guide pages |
| `src/content/docs/zh-cn/guide/` | Twelve matching Chinese Markdown guide pages |
| `src/styles/` | Shared tokens, landing styles, documentation styles |
| `public/` | Static assets |
| `scripts/*.test.mjs` | Dependency-free Node tests |
| `scripts/check-links.mjs` | Generated HTML link, asset-target, and fragment validation |

Landing pages are separate from the Starlight documentation. The English guide index at `src/content/docs/guide/index.md` builds to `/guide/`. The Chinese index builds to `/zh-cn/guide/`. Both paths are relative to the configured base. Other guide filenames become route segments, as in `guide/installation/`.

Starlight supplies navigation, theme controls, language selection, code presentation, and search. Test all five against a production build and preview; the development server does not generate the production search index.

## Local setup

Use Node 22.12.0 or later, as required by `package.json`. From the repository root:

```sh
cd site
node --version
npm ci
npm run dev
```

`npm run dev` and `npm run build` automatically fetch pinned, SHA-256-verified fonts into `public/fonts/`. The first run requires network access; later runs reuse valid local files. Font binaries are generated and ignored, while their source and license records remain in `public/fonts/README.md`.

The committed `site/package-lock.json` pins site dependencies. Install them with:

```sh
npm ci
```

The lockfile records resolved package sources. Review those URLs according to your environment before refreshing dependencies. Product use does not require Node or these site dependencies.

Open the hostname and port printed by Astro. The default base is `/`.

## Commands

Run these inside `site/`:

| Command | What it checks or starts |
| --- | --- |
| `npm run fonts` | Fetch missing or invalid pinned fonts and verify SHA-256 checksums |
| `npm run dev` | Fetch fonts, then start the Astro development server |
| `npm run build` | Fetch fonts, then create the static production build in `dist/` |
| `npm run preview` | Serve the existing build locally; build first |
| `npm test` | `node --test scripts/*.test.mjs`; no Astro install required for the current Node-only tests |
| `npm run check` | `astro check` for Astro/TypeScript diagnostics; requires dependencies |
| `npm run check:links` | Check local `href`/`src` targets and fragment IDs in `dist/`; build first |
| `npm run test:links` | Alias for the same generated-link checker |

The link checker reads `dist` by default. Pass another build directory with `node scripts/check-links.mjs PATH`. It checks local targets, base-path escapes, and fragment IDs, but does not request external websites. The Node tests cover base normalization, installer and landing wiring, and link-checker fixtures.

## GitHub Pages deployment

The workflow at `.github/workflows/pages.yml` validates, builds, and deploys the site after a documentation change reaches `main`. To start it manually, open `Actions > Deploy documentation > Run workflow`.

In the repository settings, select `Settings > Pages > Build and deployment > Source: GitHub Actions`.

Configured GitHub Pages routes:

- `https://yaojie-shen.github.io/Skills-Manager-TUI/`
- `https://yaojie-shen.github.io/Skills-Manager-TUI/zh-cn/`
- `https://yaojie-shen.github.io/Skills-Manager-TUI/guide/`
- `https://yaojie-shen.github.io/Skills-Manager-TUI/zh-cn/guide/`

Equivalent local production build:

```sh
cd site
SITE_BASE=/Skills-Manager-TUI/ SITE_URL=https://yaojie-shen.github.io npm run build
SITE_BASE=/Skills-Manager-TUI/ npm run check:links
npm run preview -- --host 127.0.0.1
```

## Validate both root and repository base paths

Run the root-path pass:

```sh
npm test
SITE_BASE=/ npm run check
SITE_BASE=/ npm run build
SITE_BASE=/ npm run check:links
SITE_BASE=/ npm run preview
```

Stop the preview server before the second pass, then rebuild and validate a repository-style prefix:

```sh
SITE_BASE=/Skills-Manager-TUI/ npm run check
SITE_BASE=/Skills-Manager-TUI/ npm run build
SITE_BASE=/Skills-Manager-TUI/ npm run check:links
SITE_BASE=/Skills-Manager-TUI/ npm run preview
```

Use the same `SITE_BASE` for the build, link check, and preview. The second build replaces `dist/`; it does not deploy the site. At the printed preview origin, open `/Skills-Manager-TUI/`, `/Skills-Manager-TUI/guide/`, and `/Skills-Manager-TUI/zh-cn/guide/`. The prefix is an example.

`SITE_BASE` accepts a path and normalizes surrounding slashes. It rejects origins, query strings, fragments, traversal segments, and unsupported segment characters. `SITE_URL` is optional and sets Astro's site origin when supplied by the maintainer. The Pages workflow supplies the configured GitHub Pages origin.

### Browser review checklist

Use this checklist after significant layout or dependency changes:

- Landing-to-guide navigation, sidebar destinations, and overview routes.
- Language switching, including whether the current topic is preserved.
- Search from the built preview, code copy buttons, theme switching, and mobile menus.
- Keyboard navigation, visible focus, skip link, main landmark, accessible button labels, and reduced-motion behavior.
- No page-level horizontal overflow; long tables and code blocks scroll within their containers.
- Images, fonts, assets, and navigation remain inside the configured prefix.
- Screen-reader behavior with assistive technology.

## Bilingual authoring and links

The paired slugs are `index`, `installation`, `quickstart`, `library`, `deployment`, `tags-presets`, `updates`, `health`, `sync`, `configuration`, `reference`, and `troubleshooting`. Each Markdown file has `title` and `description` frontmatter; Chinese pages must contain substantive Chinese explanations, not placeholders.

Inside guide Markdown, link to built routes:

```md
<!-- From guide/index.md -->
[Installation](./installation/)

<!-- From guide/quickstart.md -->
[Installation](../installation/)
[Overview](../)
```

Use the same relative forms in Chinese files. Absolute `/guide/…` links escape a configurable base, and rendered guides must not link to `.md` files. The root `README.md` links to repository source files such as `site/src/content/docs/guide/index.md`.

Keep English/Chinese guide coverage, warnings, commands, and defaults aligned. Update the root README installation snippet and `src/lib/content.ts` together when changing the public installer command.

## Generated assets

`src/generated/tui-captures.json` and `src/generated/hero-library-card.json` provide the site's TUI examples. Regenerate them with synthetic data when the TUI changes.

Silkscreen and JetBrains Mono Nerd Font are downloaded from pinned sources during development and builds. Their checksums and licenses are recorded in `public/fonts/` and `scripts/fetch-fonts.mjs`.
