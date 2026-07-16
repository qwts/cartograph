/** Single-sourced in-app help notes (#164, feeds #155's Help surface).
 *
 * Every surface renders these notes through `HelpTip` — never a second
 * hardcoded copy that could drift. Each note is 1–2 sentences of the
 * product's own honesty vocabulary, with a deep link for the full story.
 */

/** Until the in-app Help surface (#154/#155) ships, deep links land on the
 * published wiki, which is the same single-sourced content. */
export const HELP_HOME = 'https://github.com/qwts/cartograph/wiki';

export interface HelpNote {
  /** The load-bearing term, as surfaces display it. */
  term: string;
  /** 1–2 sentence in-place explanation. */
  note: string;
  /** Deep link to the fuller help content. */
  learnMoreUrl: string;
}

export const HELP_NOTES = {
  gap: {
    term: 'System gap',
    note:
      'Evidence exists but could not be resolved statically — a runtime-computed identity, a dynamic import. ' +
      'A gap is never guessed at: it truncates the trace explicitly and can be escalated tier by tier (T1–T3).',
    learnMoreUrl: HELP_HOME,
  },
  unsupported: {
    term: 'Unsupported pattern',
    note:
      'A construct no installed adapter can read — a tool limitation, never a system gap. ' +
      'It is resolved by adding an adapter (Settings → Adapters), not by escalation.',
    learnMoreUrl: HELP_HOME,
  },
  noEvidence: {
    term: 'No-evidence question',
    note:
      'A question recovery raised but found no citation for. It stays open as a question — ' +
      'Cartograph records the absence rather than inventing an answer.',
    learnMoreUrl: HELP_HOME,
  },
  tiers: {
    term: 'Confidence tiers',
    note:
      'Every fact records the tier that produced it (T0 deterministic parse up to T3 agentic proposal) ' +
      'and a confidence: Confirmed, InferredStrong, InferredWeak, or Gap. Higher tiers can never overwrite T0/T1 facts.',
    learnMoreUrl: HELP_HOME,
  },
  projection: {
    term: 'Verified-only vs best-effort',
    note:
      'Verified-only excludes InferredWeak facts from what you export; best-effort includes them, annotated. ' +
      'Explicit gaps are retained in both — the unresolved boundary survives every projection.',
    learnMoreUrl: HELP_HOME,
  },
  authority: {
    term: 'Recovery authority',
    note:
      'How much of a generated artifact rests on confirmed facts: authoritative means fully confirmed, ' +
      'partial means open findings (gaps, unsupported patterns, no-evidence questions) remain.',
    learnMoreUrl: HELP_HOME,
  },
} as const satisfies Record<string, HelpNote>;

export type HelpTopic = keyof typeof HELP_NOTES;
