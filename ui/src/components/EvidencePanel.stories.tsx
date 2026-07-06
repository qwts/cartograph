import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, within } from 'storybook/test';
import { EvidencePanel } from './EvidencePanel';
import { endpointFixture } from './EndpointsCard.stories';

// Non-ASCII content BEFORE the span makes byte offsets diverge from UTF-16
// indices — the split must be byte-accurate, so compute spans with
// TextEncoder, exactly like the provenance producer does.
const SOURCE = `// naïve café route — voilà 🚀
import express from 'express';
const app = express();
app.get('/users', listUsers);
`;

const bytes = (s: string) => new TextEncoder().encode(s).length;
const SPAN_TEXT = "app.get('/users', listUsers)";
const SPAN_START = bytes(SOURCE.slice(0, SOURCE.indexOf(SPAN_TEXT)));
const SPAN_END = SPAN_START + bytes(SPAN_TEXT);

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
    source: { text: SOURCE, window_start: 0, truncated: false },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // Byte-accurate highlight despite multibyte characters before the span.
    const mark = canvasElement.querySelector('mark');
    await expect(mark?.textContent).toBe(SPAN_TEXT);
    await expect(canvas.getByText('Confirmed')).toBeInTheDocument();
    await expect(canvas.getByText(/t0\.adapter-ts/)).toBeInTheDocument();
  },
};

export const WindowedLargeFile: Story = {
  // Large files arrive as a window; span offsets are file-absolute and must
  // be shifted by window_start before highlighting.
  args: {
    node: nodeWithSpan(),
    source: {
      text: SOURCE,
      window_start: 0,
      truncated: true,
    },
  },
  render: (args) => {
    const windowStart = 4096;
    const node = nodeWithSpan();
    const prov = node.props.prov;
    if (prov) {
      prov.evidence[0].byte_start = windowStart + SPAN_START;
      prov.evidence[0].byte_end = windowStart + SPAN_END;
    }
    return (
      <EvidencePanel
        {...args}
        node={node}
        source={{ text: SOURCE, window_start: windowStart, truncated: true }}
      />
    );
  },
  play: async ({ canvasElement }) => {
    const mark = canvasElement.querySelector('mark');
    await expect(mark?.textContent).toBe(SPAN_TEXT);
  },
};

export const Loading: Story = {
  args: { node: nodeWithSpan(), source: 'loading' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/loading source/i)).toBeInTheDocument();
  },
};

export const SourceUnavailable: Story = {
  args: { node: nodeWithSpan(), source: 'unavailable' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/source unavailable/i)).toBeInTheDocument();
  },
};

export const NoEvidence: Story = {
  args: {
    node: { id: 'mod:express', label: 'Module', props: {} },
    source: 'unavailable',
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/no evidence span/i)).toBeInTheDocument();
  },
};
