import type { SurfaceView } from './views';

/** Single-sourced help topics (#154/#155): the markdown under `docs/help/`
 * is bundled at build time (offline, no network dependency) and mirrored to
 * the wiki — `npm run check:help-mirror` fails when the two diverge. */
const RAW = import.meta.glob('../../docs/help/*.md', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

export interface HelpTopic {
  slug: string;
  title: string;
  markdown: string;
}

/** TOC order: concepts first, then the surfaces in shell order. */
const ORDER = [
  'concepts',
  'workspace',
  'ingest',
  'atlas',
  'flows',
  'spec',
  'gaps',
  'prov',
  'jobs',
  'settings',
];

export const HELP_TOPICS: HelpTopic[] = Object.entries(RAW)
  .map(([path, markdown]) => {
    const slug = path.replace(/^.*\//u, '').replace(/\.md$/u, '');
    const title = markdown.match(/^# (.+)$/mu)?.[1] ?? slug;
    return { slug, title, markdown };
  })
  .sort((a, b) => ORDER.indexOf(a.slug) - ORDER.indexOf(b.slug));

/** The help topic for a shell view — ingest's three routed views share one. */
export function topicForView(view: SurfaceView): string {
  switch (view) {
    case 'connect':
    case 'preflight':
    case 'recover':
      return 'ingest';
    default:
      return HELP_TOPICS.some((topic) => topic.slug === view) ? view : 'concepts';
  }
}
