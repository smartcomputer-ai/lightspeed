import { execFileSync } from 'node:child_process';
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import mermaid from 'astro-mermaid';
import { stageDocumentation, watchDocumentation, repositoryRoot, siteUrl } from './scripts/content.mjs';

stageDocumentation();
const gitSha = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: repositoryRoot, encoding: 'utf8' }).trim();

export default defineConfig({
  site: siteUrl,
  base: '/docs',
  trailingSlash: 'always',
  output: 'static',
  publicDir: './.generated/public',
  integrations: [
    mermaid({ autoTheme: true, enableLog: false }),
    starlight({
      title: 'Lightspeed',
      description: 'Build, deploy, and operate durable agents with Lightspeed.',
      logo: {
        dark: './src/assets/ls-mark.svg',
        light: './src/assets/ls-mark-light.svg',
        alt: '',
      },
      favicon: '/assets/ls-logo-2026-v1-ls.svg',
      customCss: ['./src/styles/theme.css'],
      social: [{ icon: 'github', label: 'Lightspeed on GitHub', href: 'https://github.com/smartcomputer-ai/lightspeed' }],
      components: {
        Head: './src/components/Head.astro',
        SocialIcons: './src/components/SocialIcons.astro',
        PageTitle: './src/components/PageTitle.astro',
        MarkdownContent: './src/components/MarkdownContent.astro',
        Footer: './src/components/Footer.astro',
      },
      head: [
        { tag: 'meta', attrs: { name: 'lightspeed-docs-sha', content: gitSha } },
        { tag: 'meta', attrs: { property: 'og:image', content: 'https://ls.bot/docs/social.png' } },
        { tag: 'meta', attrs: { name: 'twitter:card', content: 'summary_large_image' } },
      ],
      sidebar: [
        { label: 'Welcome', slug: '' },
        { label: 'Getting started', items: [
          { slug: 'getting-started/concepts' },
          { slug: 'getting-started/quickstart' },
          { slug: 'getting-started/first-agent' },
        ] },
        { label: 'Using Lightspeed', items: [
          { slug: 'using-lightspeed/sessions-and-runs' },
          { slug: 'using-lightspeed/models-and-credentials' },
          { slug: 'using-lightspeed/profiles-and-instructions' },
          { slug: 'using-lightspeed/workspaces-and-skills' },
          { slug: 'using-lightspeed/tools-and-mcp' },
          { slug: 'using-lightspeed/bots-and-triggers' },
          { slug: 'using-lightspeed/subagents-and-federation' },
          { slug: 'using-lightspeed/chat-channels' },
        ] },
        { label: 'Environments', collapsed: true, items: [
          { slug: 'environments/overview' },
          { slug: 'environments/bring-your-own-compute' },
          { slug: 'environments/incus-vms' },
          { slug: 'environments/using-environments' },
          { slug: 'environments/processes-and-jobs' },
          { slug: 'environments/credentials' },
          { slug: 'environments/power-and-cleanup' },
          { slug: 'environments/networking-and-ingress' },
        ] },
        { label: 'Access and security', collapsed: true, items: [
          { slug: 'access-and-security/overview' },
          { slug: 'access-and-security/people-and-roles' },
          { slug: 'access-and-security/private-and-shared-work' },
          { slug: 'access-and-security/api-keys-and-service-access' },
          { slug: 'access-and-security/agent-and-tool-access' },
          { slug: 'access-and-security/tenant-isolation-and-data-protection' },
        ] },
        { label: 'Deployment', items: [
          { slug: 'deployment/overview' },
          { slug: 'deployment/self-hosting' },
          { slug: 'deployment/multi-tenancy' },
          { slug: 'deployment/configuration' },
          { slug: 'deployment/operations' },
          { slug: 'deployment/upgrades-and-recovery' },
          { slug: 'deployment/troubleshooting' },
        ] },
        { label: 'Integrating and extending', collapsed: true, items: [
          { slug: 'integrating-and-extending/api-and-typescript' },
          { slug: 'integrating-and-extending/configurator-mcp' },
          { slug: 'integrating-and-extending/workflow-tools' },
          { slug: 'integrating-and-extending/custom-tools-and-model-providers' },
          { slug: 'integrating-and-extending/environment-providers' },
          { slug: 'integrating-and-extending/channel-connectors' },
        ] },
        { label: 'How it works', collapsed: true, items: [
          { slug: 'how-it-works/architecture' },
          { slug: 'how-it-works/agent-loop-and-durability' },
          { slug: 'how-it-works/context-and-storage' },
          { slug: 'how-it-works/tools-and-controller-workflows' },
        ] },
        { label: 'Development', collapsed: true, items: [
          { slug: 'development/local-development' },
          { slug: 'development/testing-and-evaluation' },
          { slug: 'development/changing-contracts' },
          { slug: 'development/contributing-and-releasing' },
        ] },
        { label: 'Reference', collapsed: true, items: [
          { label: 'JSON-RPC API', slug: 'reference/api' },
          { label: 'Environment variables', slug: 'reference/environment-variables' },
          { label: 'Workflow contract', slug: 'reference/workflow-contract' },
        ] },
      ],
    }),
  ],
  vite: { plugins: [watchDocumentation()] },
});
