import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, userEvent, waitFor, within } from 'storybook/test';
import { HELP_NOTES } from '../helpNotes';
import { HelpTip } from './HelpTip';

const meta = {
  title: 'Shell/HelpTip',
  component: HelpTip,
  args: { topic: 'gap' },
} satisfies Meta<typeof HelpTip>;

export default meta;
type Story = StoryObj<typeof meta>;

export const KeyboardReachableWithinOneInteraction: Story = {
  // AC-0088 (#164): one Tab reaches the trigger, one Enter opens the note,
  // Esc dismisses — and the text IS the single-sourced HELP_NOTES entry,
  // never a per-surface copy.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.tab();
    const trigger = canvas.getByRole('button', { name: 'What is system gap?' });
    await expect(trigger).toHaveFocus();
    await userEvent.keyboard('{Enter}');

    const note = canvas.getByRole('note');
    await expect(note).toHaveTextContent(HELP_NOTES.gap.note);
    await expect(note).toHaveTextContent('System gap.');
    await expect(canvas.getByRole('link', { name: 'Learn more' })).toHaveAttribute(
      'href',
      HELP_NOTES.gap.learnMoreUrl,
    );
    await expect(trigger).toHaveAttribute('aria-expanded', 'true');

    await userEvent.keyboard('{Escape}');
    await waitFor(() => expect(canvas.queryByRole('note')).not.toBeInTheDocument());
    await expect(trigger).toHaveAttribute('aria-expanded', 'false');
  },
};

export const HoverShowsTheNote: Story = {
  // align='end' hangs the note leftward for right-edge placements (#193
  // review): the Flow Inspector toggle sits against a clipping edge.
  args: { topic: 'projection', align: 'end' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.hover(
      canvas.getByRole('button', { name: 'What is verified-only vs best-effort?' }),
    );
    const note = canvas.getByRole('note');
    await expect(note).toHaveTextContent(HELP_NOTES.projection.note);
    await expect(note).toHaveClass('align-end');
    await userEvent.unhover(canvas.getByRole('button', { name: /verified-only/ }));
    await waitFor(() => expect(canvas.queryByRole('note')).not.toBeInTheDocument());
  },
};
