import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, within } from 'storybook/test';
import { RouteErrorBoundary } from './RouteErrorBoundary';

/** The exact failure class the boundary exists for: a surface rendering an
 *  unrenderable inspector value (handoff §Interactions #2). */
function FaultySurface(): never {
  throw new Error('span is an object, not a string');
}

const meta = {
  title: 'Shell/RouteErrorBoundary',
  component: RouteErrorBoundary,
  args: {
    view: 'atlas',
    children: <FaultySurface />,
  },
} satisfies Meta<typeof RouteErrorBoundary>;

export default meta;
type Story = StoryObj<typeof meta>;

export const CatchesSurfaceCrash: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const alert = await canvas.findByRole('alert');
    await expect(alert).toHaveTextContent('This surface failed to render');
    await expect(alert).toHaveTextContent('span is an object, not a string');
    await expect(canvas.getByRole('button', { name: 'Retry' })).toBeInTheDocument();
  },
};

export const PassesThroughHealthyChildren: Story = {
  args: { children: <p>Healthy surface content</p> },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Healthy surface content')).toBeInTheDocument();
    await expect(canvas.queryByRole('alert')).not.toBeInTheDocument();
  },
};
