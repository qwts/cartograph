import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, within } from 'storybook/test';
import { EvidencePanel } from './EvidencePanel';
import { endpointFixture } from './EndpointsCard.stories';

const SOURCE = `import express from 'express';
const app = express();
app.get('/users', listUsers);
`;

// Byte span of "app.get('/users', listUsers)" in SOURCE.
const SPAN_START = SOURCE.indexOf('app.get');
const SPAN_END = SOURCE.indexOf(');\n', SPAN_START);

function nodeWithSpan() {
  const node = endpointFixture('GET', '/users');
  const prov = node.props.prov;
  if (prov) {
    prov.evidence[0].byte_start = SPAN_START;
    prov.evidence[0].byte_end = SPAN_END;
  }
  return node;
}

const meta = {
  title: 'Atlas/EvidencePanel',
  component: EvidencePanel,
  args: { onClose: fn() },
} satisfies Meta<typeof EvidencePanel>;

export default meta;
type Story = StoryObj<typeof meta>;

export const WithSource: Story = {
  args: {
    node: nodeWithSpan(),
    source: { text: SOURCE, truncated: false },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // The evidence span is highlighted in the read-only source view.
    const mark = canvasElement.querySelector('mark');
    await expect(mark?.textContent).toBe("app.get('/users', listUsers");
    await expect(canvas.getByText('Confirmed')).toBeInTheDocument();
    await expect(canvas.getByText(/t0\.adapter-ts/)).toBeInTheDocument();
  },
};

export const SourceUnavailable: Story = {
  args: { node: nodeWithSpan(), source: null },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/source unavailable/i)).toBeInTheDocument();
  },
};

export const NoEvidence: Story = {
  args: {
    node: { id: 'mod:express', label: 'Module', props: {} },
    source: null,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/no evidence span/i)).toBeInTheDocument();
  },
};
