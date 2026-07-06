import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, within } from 'storybook/test';
import { FlowsCard } from './FlowsCard';

const SAMPLE = `# Flow dossier

## POST /orders — Verified (score 1.00)

Trigger: Endpoint \`ep:POST:/orders\`

\`\`\`mermaid
sequenceDiagram
    participant p0 as POST /orders
    participant p1 as placeOrder
    p0->>p1: HANDLES [Confirmed]
\`\`\`

| # | Hop | Tier | Confidence | Evidence |
|---|-----|------|------------|----------|
| 1 | HANDLES \`ep:POST:/orders\` → \`sym:app.ts#placeOrder\` | Deterministic | Confirmed | app.ts bytes 120..180 |

## POST /notify — Partial (score 0.50)

Trigger: Endpoint \`ep:POST:/notify\`

\`\`\`mermaid
sequenceDiagram
    participant p0 as POST /notify
    participant p1 as notify
    participant p2 as GAP: runtime-computed channel identity
    p0->>p1: HANDLES [Confirmed]
    p1--xp2: PUBLISHES [Gap]
\`\`\`

| # | Hop | Tier | Confidence | Evidence |
|---|-----|------|------------|----------|
| 1 | HANDLES \`ep:POST:/notify\` → \`sym:app.ts#notify\` | Deterministic | Confirmed | app.ts bytes 200..260 |
| 2 | PUBLISHES \`sym:app.ts#notify\` → \`gap:chan:app.ts@210\` | Deterministic | Gap | app.ts bytes 210..240 |
`;

const meta = {
  title: 'Atlas/FlowsCard',
  component: FlowsCard,
} satisfies Meta<typeof FlowsCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Empty: Story = {
  args: { dossier: '# Flow dossier\n' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/no flows traced yet/i)).toBeInTheDocument();
  },
};

export const NoBackend: Story = {
  args: { dossier: null },
};

export const Populated: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const pre = canvas.getByTestId('flows-dossier');
    // Statuses and scores are visible (R-INT-2)…
    await expect(pre.textContent).toContain('POST /orders — Verified (score 1.00)');
    // …and a Gap is visibly a Gap, truncating its flow (R-INT-4).
    await expect(pre.textContent).toContain('Partial (score 0.50)');
    await expect(pre.textContent).toContain('p1--xp2: PUBLISHES [Gap]');
    await expect(canvas.getByRole('button', { name: /copy dossier/i })).toBeEnabled();
  },
  args: { dossier: SAMPLE },
};
