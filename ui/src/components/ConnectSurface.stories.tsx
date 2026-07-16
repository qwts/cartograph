import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { ConnectSurface } from './ConnectSurface';

const meta = {
  title: 'Ingest/ConnectSurface',
  component: ConnectSurface,
  args: {
    source: 'github',
    target: '',
    canPreflight: true,
    onSourceChange: fn(),
    onTargetChange: fn(),
    onBack: fn(),
    onPreflight: fn(),
  },
} satisfies Meta<typeof ConnectSurface>;

export default meta;
type Story = StoryObj<typeof meta>;

export const PickSourceAndTarget: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // The local-only contract is stated before any work starts.
    await expect(
      canvas.getByText(/Local-only preflight\. Nothing leaves the device/),
    ).toBeInTheDocument();

    // Segmented source selector: three options, GitHub active.
    const github = canvas.getByRole('radio', { name: 'GitHub' });
    await expect(github).toHaveAttribute('aria-checked', 'true');
    await userEvent.click(canvas.getByRole('radio', { name: 'Local folder' }));
    await expect(args.onSourceChange).toHaveBeenCalledWith('local');
    await userEvent.click(canvas.getByRole('radio', { name: 'System manifest' }));
    await expect(args.onSourceChange).toHaveBeenCalledWith('manifest');

    // An empty target cannot preflight.
    await expect(canvas.getByRole('button', { name: /preflight/i })).toBeDisabled();
    await userEvent.type(canvas.getByRole('textbox'), 'github.com/acme/shop');
    await expect(args.onTargetChange).toHaveBeenCalled();

    await userEvent.click(canvas.getByRole('button', { name: 'Back' }));
    await expect(args.onBack).toHaveBeenCalled();
  },
};

export const ReadyToPreflight: Story = {
  args: { source: 'local', target: '/repos/image-trail' },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('textbox')).toHaveValue('/repos/image-trail');

    // #161: the ingest column centers in the window (auto inline margins
    // under a readable cap) instead of hugging the left edge.
    const flow = canvasElement.querySelector('.ingest-flow') as HTMLElement;
    const style = getComputedStyle(flow);
    await expect(style.maxWidth).toContain('860px');
    await expect(style.marginLeft).toBe(style.marginRight);

    await userEvent.click(canvas.getByRole('button', { name: /preflight/i }));
    await expect(args.onPreflight).toHaveBeenCalled();
  },
};

export const NoBackend: Story = {
  args: { target: 'github.com/acme/shop', canPreflight: false },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('button', { name: /preflight/i })).toBeDisabled();
  },
};
