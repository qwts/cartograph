import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, within } from 'storybook/test';
import { InvestigationActivity } from './InvestigationActivity';
import { investigationEvents } from '../investigationFixtures';

const meta = { title: 'Investigations/Activity', component: InvestigationActivity,
  args: { events: investigationEvents() } } satisfies Meta<typeof InvestigationActivity>;
export default meta;
type Story = StoryObj<typeof meta>;

export const DurableOrderedActivity: Story = {
  // AC-0190: labels reflect recorded steps; a reservation is not tool completion.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const rows = canvas.getAllByRole('listitem');
    await expect(rows).toHaveLength(9);
    await expect(rows[5]).toHaveTextContent('Source validation budget reserved');
    await expect(rows[5]).not.toHaveTextContent('Tool completed');
    await expect(rows[6]).toHaveTextContent('Tool completed');
    await expect(rows.map((row) => row.querySelector('.investigation-sequence')?.textContent))
      .toEqual(['1', '2', '3', '4', '5', '6', '7', '8', '9']);
    await expect(canvas.queryByRole('progressbar')).not.toBeInTheDocument();
  },
};

export const Empty: Story = {
  args: { events: [] },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).getByText('No activity loaded yet.')).toBeVisible();
  },
};
