// @ts-check

import starlight from '@astrojs/starlight';
import { defineConfig } from 'astro/config';

export default defineConfig({
  site: 'https://midna.mrgnhnt.com',
  // Pages merged in the October 2026 docs rewrite.
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
    starlight({
      title: 'midna',
      // The 404 page is src/pages/404.astro.
      disable404Route: true,
      description: 'A macOS terminal built for working with AI agents.',
      logo: { src: './src/assets/icon.png', alt: '' },
      favicon: '/favicon.svg',
      head: [{ tag: 'link', attrs: { rel: 'apple-touch-icon', href: '/apple-touch-icon.png' } }],
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
            { label: 'Troubleshooting', slug: 'docs/troubleshooting' },
            { label: 'Build from source', slug: 'docs/build-from-source' },
            { label: 'Changelog', link: '/changelog/' },
          ],
        },
      ],
    }),
  ],
});
