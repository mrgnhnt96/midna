// @ts-check

import { execFileSync } from 'node:child_process';
import sitemap from '@astrojs/sitemap';
import starlight from '@astrojs/starlight';
import { defineConfig } from 'astro/config';

const SITE = 'https://midna.mrgnhnt.com';
const SHARE_IMAGE = `${SITE}/share-v1.png`;
const SHARE_IMAGE_ALT = 'The midna window with example data: projects and terminals in the sidebar, and a Claude Code terminal running a migration';

const metaProperty = (property, content) => ({ tag: 'meta', attrs: { property, content } });
const metaName = (name, content) => ({ tag: 'meta', attrs: { name, content } });

const SOURCE_OF_PAGE = {
  '/': 'src/pages/index.astro',
  '/download/': 'src/pages/download.astro',
  '/changelog/': '../CHANGELOG.md',
};

function sourceOf(pathname) {
  return SOURCE_OF_PAGE[pathname] ?? `src/content/docs${pathname.replace(/\/$/, '')}.md`;
}

function docsIndexFallback(path) {
  return path === 'src/content/docs/docs.md' ? 'src/content/docs/docs/index.md' : path;
}

// The date a page's source last changed. The deploy checks out full history for this.
function lastChanged(pathname) {
  const date = execFileSync('git', ['log', '-1', '--format=%cI', '--', docsIndexFallback(sourceOf(pathname))], { encoding: 'utf8' }).trim();
  return date || undefined;
}

export default defineConfig({
  site: SITE,
  // Pages merged in the October 2026 docs rewrite.
  // Inline the small stylesheets so they don't block the first paint. Other assets keep Vite's default.
  vite: { build: { assetsInlineLimit: (file, content) => (file.endsWith('.css') ? content.length < 10240 : undefined) } },
  redirects: {
    '/docs/first-steps': '/docs/',
    '/docs/projects-and-terminals': '/docs/terminals/',
    '/docs/needs-you': '/docs/agents/',
    '/docs/insights': '/docs/agents/',
    '/docs/kass': '/docs/terminals/',
    '/docs/updates': '/docs/install/',
    '/docs/mcp': '/docs/cli/',
  },
  integrations: [
    sitemap({
      serialize(item) {
        return { ...item, lastmod: lastChanged(new URL(item.url).pathname) };
      },
    }),
    starlight({
      title: 'midna',
      // The 404 page is src/pages/404.astro.
      disable404Route: true,
      description: 'A macOS terminal built for working with AI agents.',
      logo: { src: './src/assets/icon.png', alt: 'midna icon' },
      favicon: '/favicon.svg',
      head: [
        { tag: 'link', attrs: { rel: 'icon', href: '/icon.png', type: 'image/png', sizes: '512x512' } },
        { tag: 'link', attrs: { rel: 'apple-touch-icon', href: '/apple-touch-icon.png' } },
        metaProperty('og:image', SHARE_IMAGE),
        metaProperty('og:image:width', '1200'),
        metaProperty('og:image:height', '630'),
        metaProperty('og:image:alt', SHARE_IMAGE_ALT),
        metaName('twitter:image', SHARE_IMAGE),
        metaName('twitter:image:alt', SHARE_IMAGE_ALT),
      ],
      customCss: ['./src/styles/docs.css'],
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/mrgnhnt96/midna' }],
      editLink: { baseUrl: 'https://github.com/mrgnhnt96/midna/edit/main/site/' },
      sidebar: [
        {
          label: 'Start here',
          items: [
            { label: 'Overview', slug: 'docs' },
            { label: 'Install and update', slug: 'docs/install' },
          ],
        },
        {
          label: 'Using midna',
          items: [
            { label: 'Terminals and projects', slug: 'docs/terminals' },
            { label: 'Agents and approvals', slug: 'docs/agents' },
            { label: 'Rules', slug: 'docs/rules' },
            { label: 'Triggers', slug: 'docs/triggers' },
            { label: 'Keyboard shortcuts', slug: 'docs/shortcuts' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'CLI and MCP', slug: 'docs/cli' },
            { label: 'Settings and themes', slug: 'docs/settings' },
            { label: 'Security model', slug: 'docs/security' },
            { label: 'Privacy', slug: 'privacy' },
            { label: 'Troubleshooting', slug: 'docs/troubleshooting' },
            { label: 'Build from source', slug: 'docs/build-from-source' },
            { label: 'Changelog', link: '/changelog/' },
          ],
        },
      ],
    }),
  ],
});
