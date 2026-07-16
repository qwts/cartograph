/** Human labels for the core's pipeline stages (`run_ingest` in the app
 *  crate); unknown stages fall through verbatim so a newer core still reads.
 *  Shared between RecoverSurface and JobsSurface so both surfaces describe
 *  the same stage the same way (#209). Deliberately plain language — SPEC-00's
 *  T0/T1/T2/T3 vocabulary names fact *confidence tiers*, a different axis
 *  from these pipeline *steps*, so it's kept out of this copy. */
const STAGE_LABELS: Record<string, string> = {
  scan: 'Scanning the repository',
  extract: 'Parsing source — building the import & call graph',
  load: 'Loading the graph',
  stitch: 'Linking adapters, channels & flows',
};

export function stageLabel(stage: string | null | undefined): string {
  if (!stage) return 'Preparing…';
  return STAGE_LABELS[stage] ?? stage;
}
