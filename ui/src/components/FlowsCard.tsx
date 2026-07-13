import {
  Background,
  BackgroundVariant,
  Controls,
  Handle,
  MarkerType,
  Position,
  ReactFlow,
  type Edge,
  type Node,
  type NodeProps,
  type NodeTypes,
} from '@xyflow/react';
import { useMemo, useState } from 'react';
import type { Flow, FlowHop, Tier } from '../store';

export type FlowExportMode = 'verified-only' | 'best-effort';

export interface FlowsCardProps {
  /** Traced flows as data — status and score surface per R-INT-2. */
  flows: Flow[];
  /** Flow-dossier Markdown from the spec compiler, or null with no backend. */
  dossier: string | null;
}

const STATUS_CLASS: Record<Flow['status'], string> = {
  Verified: 'tier-confirmed',
  Inferred: 'tier-inferredstrong',
  Partial: 'tier-gap',
};

const CONFIDENCE_CLASS: Record<Tier, string> = {
  Confirmed: 'tier-confirmed',
  InferredStrong: 'tier-inferredstrong',
  InferredWeak: 'tier-inferredweak',
  Gap: 'tier-gap',
};

const CONFIDENCE_COLOR: Record<Tier, string> = {
  Confirmed: '#27c93f',
  InferredStrong: '#2d9cdb',
  InferredWeak: '#f2c94c',
  Gap: '#eb5757',
};

/** Backend facts with absent/future confidence values fail closed as Gap. */
function hopConfidence(hop: FlowHop): Tier {
  return hop.confidence === 'Confirmed' ||
    hop.confidence === 'InferredStrong' ||
    hop.confidence === 'InferredWeak' ||
    hop.confidence === 'Gap'
    ? hop.confidence
    : 'Gap';
}

/** R-INT-5 projection: verified-only excludes agentic/weak hops, while
 * preserving confirmed, strong, and explicit Gap facts exactly as recorded. */
export function projectFlow(flow: Flow, mode: FlowExportMode): Flow {
  if (mode === 'best-effort') return flow;
  return {
    ...flow,
    hops: flow.hops.filter((hop) => hopConfidence(hop) !== 'InferredWeak'),
  };
}

function tierLabel(tier: string): string {
  switch (tier) {
    case 'Deterministic':
      return 'T0';
    case 'Dynamic':
      return 'T1';
    case 'Semantic':
      return 'T2';
    case 'Agentic':
      return 'T3';
    default:
      return tier || 'Unknown tier';
  }
}

function gapReason(hop: FlowHop): string {
  if (hop.gap_reason) return hop.gap_reason;
  if (/^GAP:\s*/i.test(hop.dst_name)) return hop.dst_name.replace(/^GAP:\s*/i, '') || 'unresolved';
  if (hop.confidence !== 'Gap') {
    return `confidence metadata missing or unrecognized (${hop.confidence || 'empty'})`;
  }
  return 'unresolved';
}

function attemptedEscalation(hop: FlowHop): string {
  return hop.attempted_tiers.length > 0 ? hop.attempted_tiers.join(' → ') : 'not recorded';
}

function EntityNode({ entityId, name, gapHop }: { entityId: string; name: string; gapHop: FlowHop | null }) {
  return (
    <div className={`flow-node-content ${gapHop ? 'unresolved' : ''}`}>
      <div className="flow-node-head">
        <span>{gapHop ? 'Unresolved target' : 'Flow node'}</span>
      </div>
      <strong>{name}</strong>
      {gapHop ? (
        <dl className="flow-gap-details">
          <div>
            <dt>Reason</dt>
            <dd>{gapReason(gapHop)}</dd>
          </div>
          <div>
            <dt>Attempted escalation</dt>
            <dd>{attemptedEscalation(gapHop)}</dd>
          </div>
        </dl>
      ) : (
        <span className="muted">{entityId}</span>
      )}
    </div>
  );
}

type InspectorNodeData =
  | { kind: 'trigger'; triggerKind: string; name: string }
  | { kind: 'entity'; entityId: string; name: string; gapHop: FlowHop | null };
type InspectorNode = Node<InspectorNodeData, 'inspector'>;

function InspectorNodeCard({ data }: NodeProps<InspectorNode>) {
  return (
    <>
      {data.kind === 'entity' && <Handle type="target" position={Position.Left} />}
      {data.kind === 'trigger' ? (
        <div className="flow-node-content trigger">
          <span>Trigger · {data.triggerKind}</span>
          <strong>{data.name}</strong>
        </div>
      ) : (
        <EntityNode entityId={data.entityId} name={data.name} gapHop={data.gapHop} />
      )}
      <Handle type="source" position={Position.Right} />
    </>
  );
}

const NODE_TYPES: NodeTypes = { inspector: InspectorNodeCard };

interface FlowEntity {
  id: string;
  name: string;
  gapHop: FlowHop | null;
}

function flowEntities(flow: Flow): FlowEntity[] {
  const entities = new Map<string, FlowEntity>();
  entities.set(flow.trigger, { id: flow.trigger, name: flow.trigger_name, gapHop: null });
  const add = (id: string, name: string, gapHop: FlowHop | null) => {
    const existing = entities.get(id);
    if (existing) {
      if (gapHop && !existing.gapHop) existing.gapHop = gapHop;
      return;
    }
    entities.set(id, { id, name, gapHop });
  };
  for (const hop of flow.hops) {
    add(hop.src, hop.src_name, null);
    const confidence = hopConfidence(hop);
    add(hop.dst, hop.dst_name, confidence === 'Gap' || hop.gap_reason !== null ? hop : null);
  }
  return [...entities.values()];
}

function entityDepths(flow: Flow): Map<string, number> {
  const outgoing = new Map<string, string[]>();
  for (const hop of flow.hops) {
    const destinations = outgoing.get(hop.src) ?? [];
    destinations.push(hop.dst);
    outgoing.set(hop.src, destinations);
  }
  const depths = new Map([[flow.trigger, 0]]);
  const queue = [flow.trigger];
  while (queue.length > 0) {
    const source = queue.shift();
    if (!source) break;
    const nextDepth = (depths.get(source) ?? 0) + 1;
    for (const destination of outgoing.get(source) ?? []) {
      if (depths.has(destination)) continue;
      depths.set(destination, nextDepth);
      queue.push(destination);
    }
  }
  return depths;
}

/** Build one card per recorded endpoint and connect every hop by its actual
 * src/dst ids. This must not infer sequence edges from array position. */
export function flowElements(flow: Flow): { nodes: InspectorNode[]; edges: Edge[] } {
  const entities = flowEntities(flow);
  const aliases = new Map(entities.map((entity, index) => [entity.id, `entity-${index}`]));
  const depths = entityDepths(flow);
  let fallbackDepth = Math.max(...depths.values(), 0) + 1;
  for (const entity of entities) {
    if (!depths.has(entity.id)) depths.set(entity.id, fallbackDepth++);
  }
  const columns = new Map<number, FlowEntity[]>();
  for (const entity of entities) {
    const depth = depths.get(entity.id) ?? 0;
    columns.set(depth, [...(columns.get(depth) ?? []), entity]);
  }
  const nodes: InspectorNode[] = entities.map((entity) => {
    const depth = depths.get(entity.id) ?? 0;
    const column = columns.get(depth) ?? [entity];
    const row = column.findIndex((candidate) => candidate.id === entity.id);
    const y = (row - (column.length - 1) / 2) * 190;
    const base = {
      id: aliases.get(entity.id) ?? entity.id,
      type: 'inspector',
      position: { x: depth * 470, y },
      draggable: false,
      connectable: false,
    } as const;
    if (entity.id === flow.trigger) {
      return {
        ...base,
        data: { kind: 'trigger', triggerKind: flow.trigger_kind, name: flow.trigger_name },
        className: 'flow-inspector-node trigger',
      };
    }
    return {
      ...base,
      data: { kind: 'entity', entityId: entity.id, name: entity.name, gapHop: entity.gapHop },
      className: `flow-inspector-node ${entity.gapHop ? 'unresolved' : ''}`,
    };
  });
  const edges: Edge[] = flow.hops.map((hop, index) => {
    const confidence = hopConfidence(hop);
    const gap = confidence === 'Gap' || hop.gap_reason !== null;
    return {
      id: `edge-${index}`,
      source: aliases.get(hop.src) ?? hop.src,
      target: aliases.get(hop.dst) ?? hop.dst,
      label: `${hop.label} · ${tierLabel(hop.tier)} · ${confidence}`,
      markerEnd: { type: MarkerType.ArrowClosed, color: CONFIDENCE_COLOR[confidence] },
      style: {
        stroke: CONFIDENCE_COLOR[confidence],
        strokeDasharray: gap ? '6 5' : undefined,
      },
      labelStyle: { fill: '#c1c6d5', fontSize: 10 },
      labelBgStyle: { fill: '#0e0e0e', fillOpacity: 0.92 },
      labelBgPadding: [6, 4],
      labelBgBorderRadius: 4,
    };
  });
  return { nodes, edges };
}

function mermaidSafe(value: string): string {
  return value.replace(/[\r\n;]+/g, ' ').replaceAll('"', "'");
}

function tableSafe(value: string): string {
  return value.replaceAll('|', '\\|').replace(/[\r\n]+/g, ' ');
}

/** Deterministic client projection used only when the verified-only UI mode
 * must omit InferredWeak hops from the copyable dossier (AC-0031). */
export function projectedDossier(flows: Flow[], mode: FlowExportMode): string {
  const lines = [`# Flow dossier — ${mode}`, ''];
  for (const original of flows) {
    const flow = projectFlow(original, mode);
    lines.push(`## ${flow.trigger_name} — ${flow.status} (score ${flow.score.toFixed(2)})`, '');
    lines.push(`Trigger: ${flow.trigger_kind} \`${flow.trigger}\``, '', '```mermaid', 'sequenceDiagram');
    const participants: Array<{ id: string; name: string }> = [];
    const alias = (id: string, name: string): string => {
      const existing = participants.findIndex((participant) => participant.id === id);
      if (existing >= 0) return `p${existing}`;
      participants.push({ id, name });
      return `p${participants.length - 1}`;
    };
    const arrows = flow.hops.map((hop) => {
      const source = alias(hop.src, hop.src_name);
      const target = alias(hop.dst, hop.dst_name);
      const confidence = hopConfidence(hop);
      const arrow = confidence === 'Gap' ? '--x' : '->>';
      return `    ${source}${arrow}${target}: ${hop.label} [${confidence}]`;
    });
    participants.forEach((participant, index) => lines.push(`    participant p${index} as ${mermaidSafe(participant.name)}`));
    lines.push(...arrows);
    lines.push('```', '', '| # | Hop | Tier | Confidence | Evidence |', '|---|-----|------|------------|----------|');
    flow.hops.forEach((hop, index) => {
      lines.push(
        `| ${index + 1} | ${tableSafe(hop.label)} \`${tableSafe(hop.src)}\` → \`${tableSafe(hop.dst)}\` | ${tableSafe(hop.tier)} | ${hopConfidence(hop)} | ${tableSafe(hop.evidence ?? '—')} |`,
      );
    });
    if (flow.hops.length === 0) lines.push('| — | No hops in this projection | — | — | — |');
    lines.push('');
  }
  return lines.join('\n');
}

/** M9 Flow Inspector: a selectable, read-only React Flow sequence backed by
 * the same deterministic hops as the compiler's Mermaid dossier. */
export function FlowsCard({ flows, dossier }: FlowsCardProps) {
  const [selectedTrigger, setSelectedTrigger] = useState(flows[0]?.trigger ?? '');
  const [mode, setMode] = useState<FlowExportMode>('best-effort');
  const [copied, setCopied] = useState(false);
  const selected = flows.find((flow) => flow.trigger === selectedTrigger) ?? flows[0];
  const projected = selected ? projectFlow(selected, mode) : null;
  const elements = useMemo(
    () => (projected ? flowElements(projected) : { nodes: [], edges: [] }),
    [projected],
  );
  const dossierText =
    mode === 'best-effort' && dossier?.includes('## ')
      ? dossier
      : projectedDossier(flows, mode);

  return (
    <section className="card flow-inspector-card" aria-labelledby="flow-inspector-title">
      <div className="flow-inspector-heading">
        <div>
          <h2 id="flow-inspector-title">Flow Inspector</h2>
          <p className="muted">Trace each business flow hop with its tier, evidence, and explicit unresolved boundary.</p>
        </div>
        <div className="flow-mode-toggle" aria-label="Flow export mode">
          {(['verified-only', 'best-effort'] as const).map((item) => (
            <button
              key={item}
              type="button"
              className={mode === item ? 'active' : ''}
              aria-pressed={mode === item}
              onClick={() => setMode(item)}
            >
              {item}
            </button>
          ))}
        </div>
      </div>

      {flows.length === 0 || !projected ? (
        <p className="muted flow-inspector-empty">
          No flows traced yet — ingest a repo with endpoints or event channels.
        </p>
      ) : (
        <>
          <div className="flow-inspector-toolbar">
            <label htmlFor="flow-trigger">Trigger source</label>
            <select
              id="flow-trigger"
              value={selected.trigger}
              onChange={(event) => setSelectedTrigger(event.target.value)}
            >
              {flows.map((flow) => (
                <option key={flow.trigger} value={flow.trigger}>
                  {flow.trigger_name}
                </option>
              ))}
            </select>
            <span className={`tier-badge ${STATUS_CLASS[selected.status]}`}>{selected.status}</span>
            <span className="flow-score" title="mean source-flow hop weight (SPEC-00 §5.3)">
              score {selected.score.toFixed(2)}
            </span>
            <span className="muted" role="status">
              {projected.hops.length} of {selected.hops.length} hops shown
            </span>
          </div>

          <div className="flow-inspector-canvas" data-testid="flow-inspector-canvas">
            <ReactFlow
              key={`${selected.trigger}-${mode}`}
              nodes={elements.nodes}
              edges={elements.edges}
              nodeTypes={NODE_TYPES}
              fitView
              fitViewOptions={{ padding: 0.18 }}
              minZoom={0.2}
              maxZoom={1.8}
              nodesDraggable={false}
              nodesConnectable={false}
              deleteKeyCode={null}
              proOptions={{ hideAttribution: true }}
            >
              <Controls showInteractive={false} />
              <Background variant={BackgroundVariant.Lines} gap={32} color="#414753" />
            </ReactFlow>
          </div>

          <ol className="flow-sequence" aria-label="Flow sequence">
            {projected.hops.map((hop, index) => {
              const confidence = hopConfidence(hop);
              const gap = confidence === 'Gap' || hop.gap_reason !== null;
              return (
                <li key={`${hop.src}-${hop.label}-${hop.dst}-${index}`} className={gap ? 'unresolved' : ''}>
                  <div className="flow-sequence-head">
                    <strong>{index + 1}. {gap ? 'Unresolved hop' : hop.label}</strong>
                    <span className={`tier-badge ${CONFIDENCE_CLASS[confidence]}`}>
                      {tierLabel(hop.tier)} · {confidence}
                    </span>
                  </div>
                  <span>{hop.src_name} → {hop.dst_name}</span>
                  {gap && (
                    <dl className="flow-gap-details">
                      <div><dt>Reason</dt><dd>{gapReason(hop)}</dd></div>
                      <div><dt>Attempted escalation</dt><dd>{attemptedEscalation(hop)}</dd></div>
                    </dl>
                  )}
                </li>
              );
            })}
          </ol>

          <details className="flow-dossier-details">
            <summary>Mermaid + provenance dossier ({mode})</summary>
            <pre className="evidence-source flows-dossier" data-testid="flows-dossier">
              {dossierText}
            </pre>
          </details>
          <p className="flow-copy-action">
            <button
              type="button"
              onClick={() => {
                void Promise.resolve(navigator.clipboard?.writeText(dossierText)).then(() => {
                  setCopied(true);
                  setTimeout(() => setCopied(false), 1500);
                });
              }}
            >
              {copied ? 'Copied' : `Copy ${mode} dossier`}
            </button>
          </p>
        </>
      )}
    </section>
  );
}
