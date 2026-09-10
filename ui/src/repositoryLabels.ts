import type { SystemRepo } from './store';

/** AC-0144: operational names never hide distinct current source identities.
 * Labels are display-only; matching always uses the full exact repository key. */
export function createRepositoryLabeler(
  sources: readonly Pick<SystemRepo, 'repo' | 'display_name'>[] = [],
): (repo: string) => string {
  const names = new Map(
    sources.map(({ repo, display_name }) => [repo, display_name?.trim() || repo]),
  );
  const counts = new Map<string, number>();
  for (const name of names.values()) {
    counts.set(name, (counts.get(name) ?? 0) + 1);
  }
  return (repo) => {
    const name = names.get(repo);
    if (!name || name === repo) return repo;
    return (counts.get(name) ?? 0) > 1 ? `${name} (${repo})` : name;
  };
}
