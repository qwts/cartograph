import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { HELP_TOPICS, topicForView } from '../helpTopics';
import { HelpSurface } from './HelpSurface';

const meta = {
  title: 'Shell/HelpSurface',
  component: HelpSurface,
  args: { topic: 'concepts', onTopicChange: fn() },
} satisfies Meta<typeof HelpSurface>;

export default meta;
type Story = StoryObj<typeof meta>;

export const TocCoversEverySurfaceOffline: Story = {
  // AC-0091 (#154/#155): the Help view renders entirely from bundled
  // markdown — every surface topic plus concepts, no network dependency.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const toc = within(canvas.getByRole('navigation', { name: 'Help topics' }));
    for (const title of [
      'Concepts: tiers, gaps, and honesty',
      'Workspace',
      'Ingest: Connect → Preflight → Recover',
      'Atlas',
      'Flow Inspector',
      'Spec Workbench',
      'Gaps & Drift register',
      'Provenance & Eval',
      'Jobs',
      'Settings',
    ]) {
      await expect(toc.getByRole('button', { name: title })).toBeInTheDocument();
    }
    await expect(HELP_TOPICS).toHaveLength(10);

    // The active topic renders as structured content, not raw markdown.
    await expect(
      canvas.getByRole('heading', { name: 'Concepts: tiers, gaps, and honesty' }),
    ).toBeInTheDocument();
    await expect(canvas.getByText(/higher tiers can never overwrite/i)).toBeInTheDocument();

    // TOC selection is keyboard-first: buttons all the way down.
    await userEvent.click(toc.getByRole('button', { name: 'Atlas' }));
    await expect(args.onTopicChange).toHaveBeenCalledWith('atlas');
  },
};

export const ContextualTopicsResolvePerView: Story = {
  args: { topic: 'gaps' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // AC-0092 (#155): "Help for this view" lands on the surface's topic.
    await expect(
      canvas.getByRole('heading', { name: 'Gaps & Drift register' }),
    ).toBeInTheDocument();
    await expect(canvas.getByText(/Escalate class locally/)).toBeInTheDocument();

    // The view → topic mapping is total: every routed view has a topic.
    await expect(topicForView('gaps')).toBe('gaps');
    await expect(topicForView('connect')).toBe('ingest');
    await expect(topicForView('preflight')).toBe('ingest');
    await expect(topicForView('help')).toBe('concepts');
  },
};
