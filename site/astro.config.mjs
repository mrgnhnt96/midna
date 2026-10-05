// @ts-check

import starlight from '@astrojs/starlight';
import { defineConfig } from 'astro/config';

export default defineConfig({
  site: 'https://midna.mrgnhnt.com',
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
            { label: 'Install', slug: 'docs/install' },
            { label: 'First steps', slug: 'docs/first-steps' },
          ],
        },
        {
          label: 'Using midna',
          items: [
            { label: 'Projects and terminals', slug: 'docs/projects-and-terminals' },
            { label: 'Needs you', slug: 'docs/needs-you' },
            { label: 'Rules', slug: 'docs/rules' },
            { label: 'Triggers and webhooks', slug: 'docs/triggers' },
            { label: 'Insights', slug: 'docs/insights' },
            { label: 'Dictation with Kass', slug: 'docs/kass' },
            { label: 'Keyboard shortcuts', slug: 'docs/shortcuts' },
          ],
        },
        {
          label: 'For agents',
          items: [
            { label: 'Claude Code and Codex', slug: 'docs/agents' },
            { label: 'The midna CLI', slug: 'docs/cli' },
            { label: 'MCP server', slug: 'docs/mcp' },
          ],
        },
        {
          label: 'Setup',
          items: [
            { label: 'Settings', slug: 'docs/settings' },
            { label: 'Updates', slug: 'docs/updates' },
            { label: 'Security model', slug: 'docs/security' },
            { label: 'Troubleshooting', slug: 'docs/troubleshooting' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'Build from source', slug: 'docs/build-from-source' },
            { label: 'Changelog', link: '/changelog/' },
            {
              label: 'Report an issue',
              link: 'https://github.com/mrgnhnt96/midna/issues/new/choose',
              attrs: { target: '_blank' },
            },
          ],
        },
      ],
    }),
  ],
});
