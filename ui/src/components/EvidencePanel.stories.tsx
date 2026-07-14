import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
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
    await expect(canvas.getAllByText('Confirmed').length).toBeGreaterThan(0);
    await expect(canvas.getByText(/t0\.adapter-ts/)).toBeInTheDocument();

    // The full span range includes the end line:col — the span sits on
    // line 4 of the fixture (multibyte-safe columns).
    await expect(canvas.getByTestId('span-range')).toHaveTextContent(
      `bytes ${SPAN_START}–${SPAN_END} · L4:1 – L4:${SPAN_TEXT.length + 1}`,
    );
    // True line numbers render in the gutter (1-based from the window).
    await expect(canvasElement.querySelector('.evidence-gutter')?.textContent).toContain('4');

    // Why-this-tier explanation is available on demand.
    await userEvent.click(canvas.getByText('Why this tier?'));
    await expect(canvas.getByText(/Established deterministically/)).toBeInTheDocument();

    // The full 64-hex hash renders, and copy carries exactly that value.
    const hash = canvas.getByTestId('content-hash').textContent;
    await expect(hash).toMatch(/^[0-9a-f]{64}$/);
    const copy = canvas.getByRole('button', { name: 'Copy' });
    await expect(copy).toHaveAttribute('data-hash', hash);

    // Integrity footer is always present.
    await expect(
      canvas.getByText(/T2\/T3 never overwrite or masquerade as T0\/T1/),
    ).toBeInTheDocument();
  },
};

export const GapOffersResolutionStrategy: Story = {
  args: {
    node: (() => {
      const node = nodeWithSpan();
      if (node.props.prov) node.props.prov.confidence_tier = 'Gap';
      return node;
    })(),
    source: { text: SOURCE, window_start: 0, truncated: false },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // The why-strip names the honesty contract.
    await expect(
      canvas.getByText(/Why this is a Gap: evidence exists but is not statically resolvable/),
    ).toBeInTheDocument();
    // The CTA exists; without a runner (pre-#120) it is disabled, not absent.
    await expect(
      canvas.getByRole('button', { name: 'Open Resolution Strategy' }),
    ).toBeDisabled();
  },
};

export const SupportingEvidenceNavigates: Story = {
  args: {
    node: (() => {
      const node = nodeWithSpan();
      node.props.prov?.evidence.push({
        repo: 'local',
        path: 'src/routes.ts',
        byte_start: 10,
        byte_end: 42,
        commit_sha: 'workdir',
      });
      return node;
    })(),
    source: { text: SOURCE, window_start: 0, truncated: false },
    onShowEvidence: fn(),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Supporting evidence')).toBeInTheDocument();
    await userEvent.click(canvas.getByText(/src\/routes\.ts · bytes 10–42/));
    await expect(args.onShowEvidence).toHaveBeenCalledWith(1);
  },
};

export const WindowedLineNumbersAreAbsolute: Story = {
  // A windowed file must show true file line numbers, not restart at 1.
  args: {
    node: nodeWithSpan(),
    source: { text: SOURCE, window_start: 0, window_start_line: 120, truncated: true },
  },
  play: async ({ canvasElement }) => {
    const gutter = canvasElement.querySelector('.evidence-gutter');
    await expect(gutter?.textContent?.startsWith('120')).toBe(true);
    await expect(within(canvasElement).getByTestId('span-range')).toHaveTextContent('L123:1');
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
