import { describe, expect, it } from 'vitest';
import { assignBands } from './atlasLayout';
import type { AtlasSnapshot, GraphEdge, GraphNode } from './store';

function node(id: string, label: string, props: GraphNode['props'] = {}): GraphNode {
  return { id, label, props };
}

function edge(src: string, dst: string, label: string): GraphEdge {
  return { src, dst, label, props: {} };
}

// #244 review (PRRT_kwDOTOhqY86l15UK): a `.ts`-authored config (vite.config.ts,
// …) is re-parsed by the TS adapter, whose own File node wins over the
// toolchain's and carries only `path`/`prov` — never `props.config`. The
// Tool --DEFINED_IN--> File edge the toolchain always emits is the fallback
// that keeps that File in Tools/Build beside its Tool instead of Server.
describe('assignBands — Tools/Build config File placement', () => {
  it('bands an adapter-owned config File (no props.config) via its Tool DEFINED_IN edge', () => {
    const snapshot: AtlasSnapshot = {
      nodes: [
        node('tool:local/app@vite.config.ts', 'Tool'),
        // Adapter-owned File node: only path/prov, no `config` prop
        // (adapters-lang-ts/src/lib.rs File-node emission for every source file).
        node('file:local/app@vite.config.ts', 'File', { path: 'vite.config.ts' }),
      ],
      edges: [edge('tool:local/app@vite.config.ts', 'file:local/app@vite.config.ts', 'DEFINED_IN')],
    };
    const bands = assignBands(snapshot);
    expect(bands.get('tool:local/app@vite.config.ts')).toBe('tools');
    expect(bands.get('file:local/app@vite.config.ts')).toBe('tools');
  });

  it('still bands a toolchain-emitted config File via props.config alone', () => {
    const snapshot: AtlasSnapshot = {
      nodes: [node('file:local/app@tsconfig.json', 'File', { path: 'tsconfig.json', config: true })],
      edges: [],
    };
    const bands = assignBands(snapshot);
    expect(bands.get('file:local/app@tsconfig.json')).toBe('tools');
  });

  it('leaves a plain source File in Server', () => {
    const snapshot: AtlasSnapshot = {
      nodes: [node('file:local/app@src/index.ts', 'File', { path: 'src/index.ts' })],
      edges: [],
    };
    const bands = assignBands(snapshot);
    expect(bands.get('file:local/app@src/index.ts')).toBe('server');
  });

  it('does not band a File in Tools/Build via an unrelated DEFINED_IN edge from a Symbol', () => {
    const snapshot: AtlasSnapshot = {
      nodes: [
        node('sym:local/app@src/index.ts#handler', 'Symbol'),
        node('file:local/app@src/index.ts', 'File', { path: 'src/index.ts' }),
      ],
      edges: [edge('sym:local/app@src/index.ts#handler', 'file:local/app@src/index.ts', 'DEFINED_IN')],
    };
    const bands = assignBands(snapshot);
    expect(bands.get('file:local/app@src/index.ts')).toBe('server');
  });
});
