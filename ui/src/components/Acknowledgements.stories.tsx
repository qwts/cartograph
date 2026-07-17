import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, within } from 'storybook/test';
import Acknowledgements from './Acknowledgements';

const meta = {
  title: 'Settings/Acknowledgements',
  component: Acknowledgements,
} satisfies Meta<typeof Acknowledgements>;

export default meta;
type Story = StoryObj<typeof meta>;

// The in-app open-source licenses view (#222): renders the generated
// third-party notices under the project's own PolyForm license statement.
export const Default: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // Intro line — phrase unique to the component (the notices body also
    // mentions PolyForm, so match text that only the intro paragraph carries).
    await expect(
      canvas.getByText(/incorporates the third-party open-source software/),
    ).toBeInTheDocument();
    // The generated notices are embedded verbatim in the scrollable block.
    const notices = canvasElement.querySelector('pre.acknowledgements-text');
    await expect(notices).not.toBeNull();
    await expect(notices?.textContent ?? '').toMatch(/Third-Party Notices/);
  },
};
