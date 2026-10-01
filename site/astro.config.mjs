import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import { normalizeBase } from './src/lib/base.mjs';

// Keep local preview at /. Set SITE_BASE=/Skills-Manager-TUI/ for a project site.
const base = normalizeBase(process.env.SITE_BASE);
const page = (label, slug, zh) => ({ label, slug: slug === 'index' ? 'guide' : `guide/${slug}`, translations: { 'zh-CN': zh } });

export default defineConfig({
  ...(process.env.SITE_URL ? { site: process.env.SITE_URL } : {}),
  base,
  trailingSlash: 'always',
  integrations: [
    starlight({
      title: 'Skills Manager',
      description: 'One Library. Your agents. A terminal-first workflow.',
      defaultLocale: 'root',
      locales: {
        root: { label: 'English', lang: 'en' },
        'zh-cn': { label: '简体中文', lang: 'zh-CN' },
      },
      favicon: '/favicon.svg',
      components: {
        SiteTitle: './src/components/DocsSiteTitle.astro',
        ThemeSelect: './src/components/DocsThemeToggle.astro',
        LanguageSelect: './src/components/DocsLanguageMenu.astro',
        Footer: './src/components/DocsFooter.astro',
      },
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/yaojie-shen/Skills-Manager-TUI' }],
            customCss: ['./src/styles/docs.css'],
      sidebar: [
        {
          label: 'Start here', translations: { 'zh-CN': '从这里开始' },
          items: [page('Overview', 'index', '概览'), page('Installation', 'installation', '安装'), page('Quick start', 'quickstart', '快速开始')],
        },
        {
          label: 'Your workflow', translations: { 'zh-CN': '日常工作流' },
          items: [page('Library & sources', 'library', 'Library 与来源'), page('Agent deployment', 'deployment', 'Agent 部署'), page('Tags & presets', 'tags-presets', '标签与预设'), page('Updates & baselines', 'updates', '更新与基线'), page('Health & repair', 'health', '健康检查与修复'), page('Root backup & sync', 'sync', '根目录备份与同步')],
        },
        {
          label: 'Reference', translations: { 'zh-CN': '参考手册' },
          items: [page('Configuration', 'configuration', '配置'), page('CLI & TUI reference', 'reference', 'CLI 与 TUI 参考'), page('Troubleshooting', 'troubleshooting', '故障排查')],
        },
      ],
    }),
  ],
});
