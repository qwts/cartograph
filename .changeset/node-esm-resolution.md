---
"cartograph": minor
---

Imports now resolve the way Node and TypeScript actually resolve them: directory index files (`./utils` → `utils/index.ts`), the NodeNext `.js`-extension idiom (`import './foo.js'` → the `foo.ts` source), tsconfig/jsconfig `paths` and `baseUrl` aliases, and workspace-package names via their `exports` maps — each resolution proven against real files and citing the deciding config file as evidence. Unresolvable specifiers stay explicitly unresolved, and external packages are never guessed into.
