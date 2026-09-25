import type { SpecArtifact } from './store';

export type ReadSpecArtifact = (artifactId: string) => Promise<SpecArtifact | null>;

/**
 * Fills in any artifact whose `export_spec` preview was capped (#488) with
 * its full content/assertions before it leaves the app (export or copy).
 *
 * `read_spec_artifact` is a `Result`-typed Tauri command, never `Option`, so
 * a `null` return from `readSpecArtifact` always means the on-demand fetch
 * itself failed — the store's `readSpecArtifact` action already records
 * `specError` in that case — never a legitimate empty result. Returning
 * `null` here tells the caller to abort the whole action instead of
 * silently substituting the capped preview, which would hand out incomplete
 * content/assertions under an action presented as complete (PR #533 review
 * thread).
 */
export async function withFullArtifacts(
  artifacts: SpecArtifact[],
  readSpecArtifact: ReadSpecArtifact,
): Promise<SpecArtifact[] | null> {
  const resolved = await Promise.all(
    artifacts.map(async (artifact) => {
      if (!artifact.content_truncated && !artifact.assertions_truncated) return artifact;
      return readSpecArtifact(artifact.id);
    }),
  );
  if (resolved.some((artifact) => artifact === null)) return null;
  return resolved as SpecArtifact[];
}

/**
 * Resolves the artifact whose content should be copied for the "Copy
 * artifact" action — the preview as-is when it was never truncated, or the
 * full artifact fetched on demand. Returns `null` to tell the caller to
 * abort instead of copying the capped preview — see `withFullArtifacts`.
 */
export async function resolveArtifactForCopy(
  artifact: SpecArtifact,
  readSpecArtifact: ReadSpecArtifact,
): Promise<SpecArtifact | null> {
  if (!artifact.content_truncated) return artifact;
  return readSpecArtifact(artifact.id);
}
