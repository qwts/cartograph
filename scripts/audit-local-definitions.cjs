#!/usr/bin/env node
'use strict';

// Independent source-only syntax/binding assessment for SPEC-08 / AC-0171.
// Usage: node scripts/audit-local-definitions.cjs SOURCE_ROOT INPUT_MANIFEST OUTPUT_DIR [AUDIT_JSON]
// Reads only explicitly listed staged sources and OUTPUT_DIR/graph.json. The
// TypeScript parser/binder never executes target code or resolves its imports.
// This is not a runtime, complete-input-closure, secrecy, or capture-receipt proof.
// Limits bound assessment inputs/work; they are not a hostile-parser sandbox.

const fs = require('node:fs');
const path = require('node:path');
const { isDeepStrictEqual } = require('node:util');

const EXPECTED_TYPESCRIPT_VERSION = '6.0.3';
const LIMITS = Object.freeze({
  manifestBytes: 256 * 1024,
  graphBytes: 64 * 1024 * 1024,
  sourceFiles: 256,
  sourceFileBytes: 2 * 1024 * 1024,
  totalSourceBytes: 32 * 1024 * 1024,
  astNodes: 500_000,
  astDepth: 512,
  graphNodes: 100_000,
  graphEdges: 500_000,
  definitionsPerRule: 16,
  arenaNodesPerRule: 64,
  initializerDependenciesPerRule: 128,
  dependenciesPerRule: 128,
  usesPerDefinition: 256,
  expressionBytes: 8 * 1024,
  diagnostics: 256,
});

class AuditFailure extends Error {
  constructor(reason, citation = null) {
    super(reason);
    this.reason = reason;
    this.citation = citation;
  }
}

function requireInput(condition, reason, citation = null) {
  if (!condition) throw new AuditFailure(reason, citation);
}

function boundedString(value, maxBytes) {
  return typeof value === 'string' && Buffer.byteLength(value, 'utf8') <= maxBytes;
}

function arrayWithin(value, maximum) {
  return Array.isArray(value) && value.length <= maximum;
}

function readBoundedFile(filename, maximum) {
  // No symlink final entries and no special-file open before the type check.
  const named = fs.lstatSync(filename);
  requireInput(named.isFile() && named.size <= maximum, 'input_file_type_or_size');
  const fd = fs.openSync(filename, fs.constants.O_RDONLY |
    (fs.constants.O_NOFOLLOW || 0) | (fs.constants.O_NONBLOCK || 0));
  try {
    const opened = fs.fstatSync(fd);
    requireInput(
      opened.isFile() && opened.dev === named.dev && opened.ino === named.ino,
      'input_file_identity_changed',
    );
    // Allocate at most maximum+1 even if another process grows the file.
    const buffer = Buffer.alloc(Math.min(opened.size, maximum) + 1);
    let used = 0;
    while (used < buffer.length) {
      const count = fs.readSync(fd, buffer, used, buffer.length - used, null);
      if (count === 0) break;
      used += count;
    }
    requireInput(used === opened.size && used <= maximum, 'input_file_changed_or_oversized');
    const after = fs.fstatSync(fd);
    requireInput(
      after.size === opened.size && after.mtimeMs === opened.mtimeMs,
      'input_file_changed_during_read',
    );
    return buffer.subarray(0, used);
  } finally {
    fs.closeSync(fd);
  }
}

function decodeUtf8(buffer) {
  const text = buffer.toString('utf8');
  requireInput(Buffer.from(text, 'utf8').equals(buffer), 'input_not_exact_utf8');
  return text;
}

function readJson(filename, maximum) {
  return JSON.parse(decodeUtf8(readBoundedFile(filename, maximum)));
}

function stagedPath(root, relative) {
  requireInput(
    boundedString(relative, 4096) && relative.length > 0 && !relative.includes('\\') &&
      !relative.includes('\0') && !path.isAbsolute(relative),
    'invalid_manifest_source_path',
  );
  const parts = relative.split('/');
  requireInput(parts.every(part => part && part !== '.' && part !== '..'), 'invalid_manifest_source_path');
  let filename = root;
  for (let i = 0; i < parts.length; i += 1) {
    filename = path.join(filename, parts[i]);
    const metadata = fs.lstatSync(filename);
    requireInput(!metadata.isSymbolicLink(), 'symlink_in_staged_source_path');
    requireInput(i === parts.length - 1 || metadata.isDirectory(), 'invalid_staged_source_directory');
  }
  return filename;
}

function createAudit(ts, sourceRoot, manifest, graph) {
  requireInput(
    boundedString(manifest.repo, 256) && manifest.repo.length > 0 &&
      boundedString(manifest.commit_sha, 256) && manifest.commit_sha.length > 0 &&
      arrayWithin(manifest.source_files, LIMITS.sourceFiles) && manifest.source_files.length > 0,
    'invalid_input_manifest',
  );
  requireInput(new Set(manifest.source_files).size === manifest.source_files.length, 'duplicate_input_source');
  requireInput(
    arrayWithin(graph.nodes, LIMITS.graphNodes) && arrayWithin(graph.edges, LIMITS.graphEdges),
    'invalid_graph_or_graph_limit',
  );

  const buffers = new Map();
  const sourceFiles = new Map();
  const offsets = new Map();
  let totalBytes = 0;
  for (const relative of manifest.source_files) {
    const buffer = readBoundedFile(stagedPath(sourceRoot, relative), LIMITS.sourceFileBytes);
    totalBytes += buffer.length;
    requireInput(totalBytes <= LIMITS.totalSourceBytes, 'total_source_byte_limit');
    const text = decodeUtf8(buffer);
    const extension = path.extname(relative).toLowerCase();
    requireInput(['.ts', '.tsx', '.js', '.jsx', '.mts', '.cts', '.mjs', '.cjs'].includes(extension), 'unsupported_source_file_kind');
    const kind = extension === '.tsx' ? ts.ScriptKind.TSX : extension === '.jsx' ? ts.ScriptKind.JSX :
      ['.js', '.mjs', '.cjs'].includes(extension) ? ts.ScriptKind.JS : ts.ScriptKind.TS;
    const file = ts.createSourceFile(relative, text, ts.ScriptTarget.Latest, true, kind);
    requireInput(file.parseDiagnostics.length === 0, 'source_parse_error');
    buffers.set(relative, buffer);
    sourceFiles.set(relative, file);
    // Precompute UTF-16 -> UTF-8 offsets once, avoiding quadratic prefix scans.
    const byteOffsets = new Uint32Array(text.length + 1);
    byteOffsets.fill(0xffffffff);
    let utf16 = 0;
    let bytes = 0;
    for (const character of text) {
      byteOffsets[utf16] = bytes;
      utf16 += character.length;
      bytes += Buffer.byteLength(character, 'utf8');
    }
    byteOffsets[utf16] = bytes;
    offsets.set(file, byteOffsets);
  }

  // No filesystem fallback, default library, import resolution, or target emit.
  const host = {
    getSourceFile: name => sourceFiles.get(name),
    getDefaultLibFileName: () => '',
    writeFile: () => { throw new AuditFailure('unexpected_target_emit'); },
    getCurrentDirectory: () => '',
    getDirectories: () => [],
    fileExists: name => sourceFiles.has(name),
    readFile: name => sourceFiles.get(name)?.text,
    getCanonicalFileName: name => name,
    useCaseSensitiveFileNames: () => true,
    getNewLine: () => '\n',
  };
  const program = ts.createProgram([...sourceFiles.keys()], { noLib: true, noResolve: true, allowJs: true }, host);
  const checker = program.getTypeChecker();
  const spanCache = new WeakMap();
  function span(file, node) {
    if (!node) return null;
    if (!spanCache.has(node)) {
      spanCache.set(node, {
        byte_start: offsets.get(file)[node.getStart(file)],
        byte_end: offsets.get(file)[node.end],
      });
    }
    return spanCache.get(node);
  }
  const sameSpan = (a, b) => !!a && !!b && a.byte_start === b.byte_start && a.byte_end === b.byte_end;
  const sameSource = (a, b) => sameSpan(a, b) && a.repo === b.repo && a.path === b.path && a.commit_sha === b.commit_sha;
  const maps = new Map();
  let astNodes = 0;
  for (const [relative, file] of sourceFiles) {
    const map = new Map();
    const pending = [{ node: file, depth: 0 }];
    while (pending.length) {
      const { node, depth } = pending.pop();
      astNodes += 1;
      requireInput(astNodes <= LIMITS.astNodes && depth <= LIMITS.astDepth, 'source_ast_work_limit');
      const range = span(file, node);
      const key = `${range.byte_start}:${range.byte_end}`;
      if (!map.has(key)) map.set(key, []);
      map.get(key).push(node);
      ts.forEachChild(node, child => { pending.push({ node: child, depth: depth + 1 }); });
    }
    maps.set(relative, map);
  }
  function ast(reference, predicate = () => true) {
    return maps.get(reference?.path)?.get(`${reference?.byte_start}:${reference?.byte_end}`)?.find(predicate);
  }
  function owner(node) {
    while (node) {
      if (ts.isFunctionLike(node)) return node;
      node = node.parent;
    }
    return null;
  }

  const issues = [];
  let issueCount = 0;
  const gapIds = new Set(graph.nodes
    .filter(node => node?.label === 'Gap' && boundedString(node.id, 8192))
    .map(node => node.id));
  const dependencyEdges = new Map();
  for (const edge of graph.edges) {
    if (edge?.label !== 'DEPENDS_ON' || !boundedString(edge.src, 8192) || !boundedString(edge.dst, 8192)) continue;
    if (!dependencyEdges.has(edge.src)) dependencyEdges.set(edge.src, new Set());
    dependencyEdges.get(edge.src).add(edge.dst);
  }
  const counts = {
    rules: 0, definitions: 0, uses: 0, source_refs: 0, expressions: 0,
    complete_syntax_expressions: 0, redacted_expressions: 0, unsupported_expressions: 0,
    arena_nodes: 0, initializer_dependencies: 0, admitted_uses_symbol_resolved: 0,
    admitted_uses_order_verified: 0, unsupported_nodes: 0,
    explicit_gap_links_checked: 0,
  };
  function citation(reference) {
    // Never echo displays, raw source, arbitrary IDs, parser messages or stacks.
    // Only recognized input metadata and bounded numeric ranges can be reported.
    if (!reference || !buffers.has(reference.path)) return null;
    return {
      repo: manifest.repo, commit_sha: manifest.commit_sha, path: reference.path,
      byte_start: Number.isSafeInteger(reference.byte_start) ? reference.byte_start : null,
      byte_end: Number.isSafeInteger(reference.byte_end) ? reference.byte_end : null,
    };
  }
  function check(condition, reason, reference) {
    if (condition) return true;
    issueCount += 1;
    if (issues.length < LIMITS.diagnostics) issues.push({ reason, citation: citation(reference) });
    return false;
  }
  function source(reference) {
    counts.source_refs += 1;
    const buffer = buffers.get(reference?.path);
    const start = reference?.byte_start;
    const end = reference?.byte_end;
    return check(
      reference?.repo === manifest.repo && reference?.commit_sha === manifest.commit_sha && buffer &&
        Number.isSafeInteger(start) && Number.isSafeInteger(end) && start >= 0 && end > start && end <= buffer.length &&
        (buffer[start] & 0xc0) !== 0x80 && (end === buffer.length || (buffer[end] & 0xc0) !== 0x80),
      'source_identity_or_bounds', reference,
    );
  }
  function expression(value) {
    requireInput(value && boundedString(value.display, LIMITS.expressionBytes) &&
      ['complete_syntax', 'redacted', 'unsupported'].includes(value.capture), 'invalid_expression_envelope', citation(value?.source));
    counts.expressions += 1;
    counts[`${value.capture}_expressions`] += 1;
    if (source(value.source) && value.capture === 'complete_syntax') {
      const bytes = buffers.get(value.source.path).subarray(value.source.byte_start, value.source.byte_end);
      check(value.display === bytes.toString('utf8'), 'complete_display_mismatch', value.source);
    }
    check(!!ast(value.source), 'expression_ast_span_missing', value.source);
  }
  function dependencySources(dependency) {
    requireInput(dependency && dependency.resolution, 'invalid_dependency_envelope');
    source(dependency.source);
    if (dependency.resolution.kind === 'binding') source(dependency.resolution.declaration);
  }
  function gapLink(gapId, factId, rule, reference) {
    counts.explicit_gap_links_checked += 1;
    check(boundedString(gapId, 8192) && rule.interpretation.gap_ids.includes(gapId) &&
      gapIds.has(gapId) && dependencyEdges.get(factId)?.has(gapId),
    'missing_explicit_gap_dependency_link', reference);
  }

  const binaryOperators = {
    '==': 'loose_equal', '!=': 'loose_not_equal', '===': 'strict_equal', '!==': 'strict_not_equal',
    '<': 'less_than', '<=': 'less_than_or_equal', '>': 'greater_than', '>=': 'greater_than_or_equal',
    '+': 'add', '-': 'subtract', '*': 'multiply', '/': 'divide', '%': 'remainder', '**': 'exponent',
    '<<': 'left_shift', '>>': 'right_shift', '>>>': 'unsigned_right_shift', '&': 'bitwise_and',
    '|': 'bitwise_or', '^': 'bitwise_xor', in: 'in', instanceof: 'instanceof',
    '&&': 'and', '||': 'or', '??': 'nullish',
  };
  const unaryOperators = { '!': 'not', '+': 'plus', '-': 'minus', '~': 'bitwise_not', typeof: 'typeof', void: 'void' };

  function arenaShape(entry, definition, rule, file) {
    const shape = entry.kind;
    const node = ast(entry.expression.source);
    if (!node || !shape) return false;
    const child = (index, expected) => Number.isSafeInteger(index) && index >= 0 && expected &&
      sameSpan(definition.expression.nodes[index]?.expression?.source, span(file, expected));
    const dependency = Number.isSafeInteger(shape.dependency) && shape.dependency >= 0
      ? definition.dependencies[shape.dependency] : null;
    switch (shape.kind) {
      case 'unsupported':
        counts.unsupported_nodes += 1;
        return rule.interpretation.gap_ids.includes(shape.gap_id);
      case 'literal':
        return ts.isStringLiteral(node) || ts.isNumericLiteral(node) ||
          [ts.SyntaxKind.TrueKeyword, ts.SyntaxKind.FalseKeyword, ts.SyntaxKind.NullKeyword].includes(node.kind);
      case 'identifier':
        return (ts.isIdentifier(node) || node.kind === ts.SyntaxKind.ThisKeyword) &&
          sameSource(dependency?.source, entry.expression.source) &&
          ['binding', 'unresolved'].includes(dependency?.resolution?.kind);
      case 'property_name':
        return ts.isIdentifier(node) && ts.isPropertyAccessExpression(node.parent) && node.parent.name === node;
      case 'member':
        return (ts.isPropertyAccessExpression(node) || ts.isElementAccessExpression(node)) &&
          shape.computed === ts.isElementAccessExpression(node) && shape.optional === !!node.questionDotToken &&
          child(shape.object, node.expression) && child(shape.property, node.name || node.argumentExpression) &&
          sameSource(dependency?.source, entry.expression.source) &&
          dependency?.resolution?.kind === 'unresolved';
      case 'parenthesized':
        return ts.isParenthesizedExpression(node) && child(shape.value, node.expression);
      case 'binary':
      case 'logical':
        return ts.isBinaryExpression(node) && binaryOperators[node.operatorToken.getText(file)] === shape.operator &&
          (shape.kind === 'logical') === ['&&', '||', '??'].includes(node.operatorToken.getText(file)) &&
          child(shape.left, node.left) && child(shape.right, node.right);
      case 'unary': {
        const operator = ts.isPrefixUnaryExpression(node) ? ts.tokenToString(node.operator) :
          ts.isTypeOfExpression(node) ? 'typeof' : ts.isVoidExpression(node) ? 'void' : null;
        return unaryOperators[operator] === shape.operator && child(shape.operand, node.operand || node.expression);
      }
      default:
        return false;
    }
  }

  for (const fact of graph.nodes) {
    if (fact?.label !== 'BusinessRule') continue;
    const rule = fact.props?.rule;
    counts.rules += 1;
    requireInput(boundedString(fact.id, 8192) && rule?.schema_version === 2 && arrayWithin(rule.local_definitions, LIMITS.definitionsPerRule) &&
      arrayWithin(rule.dependencies, LIMITS.dependenciesPerRule) &&
      arrayWithin(rule.interpretation?.gap_ids, LIMITS.dependenciesPerRule), 'invalid_v2_rule_envelope', citation(rule?.exit_source));
    check(rule.interpretation.execution_predicate === 'not_established' &&
      rule.interpretation.consumer_effect === 'not_established', 'authority', rule.exit_source);
    const exit = ast(rule.exit_source);
    const allDependencies = [...rule.dependencies];
    let ruleNodes = 0;
    let ruleDependencies = 0;
    for (const definition of rule.local_definitions) {
      requireInput(definition && arrayWithin(definition.expression?.nodes, LIMITS.arenaNodesPerRule) &&
        definition.expression.nodes.length > 0 && arrayWithin(definition.dependencies, LIMITS.initializerDependenciesPerRule) &&
        arrayWithin(definition.uses, LIMITS.usesPerDefinition) && definition.uses.length > 0 &&
        Number.isSafeInteger(definition.expression.root) && definition.expression.root >= 0 &&
        definition.expression.root < definition.expression.nodes.length && boundedString(definition.binding_id, 8192),
      'invalid_definition_envelope', citation(definition?.declaration));
      ruleNodes += definition.expression.nodes.length;
      ruleDependencies += definition.dependencies.length;
      allDependencies.push(...definition.dependencies);
    }
    requireInput(ruleNodes <= LIMITS.arenaNodesPerRule && ruleDependencies <= LIMITS.initializerDependenciesPerRule,
      'definition_rule_limit', citation(rule.exit_source));

    for (const definition of rule.local_definitions) {
      counts.definitions += 1;
      const localSources = [definition.initializer?.source, ...definition.uses,
        ...definition.expression.nodes.map(entry => entry?.expression?.source),
        ...definition.dependencies.flatMap(dependency => dependency?.resolution?.kind === 'binding'
          ? [dependency.source, dependency.resolution.declaration] : [dependency?.source])];
      check(localSources.every(reference => reference?.path === definition.declaration?.path),
        'definition_crosses_source_file', definition.declaration);
      source(definition.declaration);
      definition.uses.forEach(source);
      definition.dependencies.forEach(dependencySources);
      expression(definition.initializer);
      for (const entry of definition.expression.nodes) {
        counts.arena_nodes += 1;
        expression(entry?.expression);
      }
      const declaration = ast(definition.declaration, ts.isVariableDeclaration);
      const file = sourceFiles.get(definition.declaration?.path);
      if (!check(!!declaration, 'declaration_ast_span_missing', definition.declaration)) continue;
      const statement = declaration.parent?.parent;
      const validConst = ts.isIdentifier(declaration.name) && declaration.initializer &&
        !!(declaration.parent.flags & ts.NodeFlags.Const) && ts.isVariableStatement(statement) && ts.isBlock(statement.parent);
      if (!check(validConst, 'const_statement', definition.declaration)) continue;
      check(sameSpan(span(file, declaration.initializer), definition.initializer.source), 'initializer_span', definition.initializer.source);
      check(definition.binding_id === `binding:${manifest.repo}@${definition.declaration.path}#declaration@${definition.declaration.byte_start}`,
        'binding_id', definition.declaration);
      check(!!exit && owner(declaration) === owner(exit), 'owner_containment', definition.declaration);

      for (const use of definition.uses) {
        counts.uses += 1;
        const read = ast(use, ts.isIdentifier);
        if (!check(!!read && read.text === declaration.name.text, 'use_identifier', use)) continue;
        const symbol = checker.getSymbolAtLocation(read);
        if (check(!!symbol?.declarations?.includes(declaration), 'use_symbol', use)) counts.admitted_uses_symbol_resolved += 1;
        let containingStatement = read;
        while (containingStatement && containingStatement.parent !== statement.parent) containingStatement = containingStatement.parent;
        const ordered = !!containingStatement && containingStatement !== statement &&
          statement.end <= containingStatement.getStart(file) && owner(read) === owner(declaration);
        if (check(ordered, 'use_scope_order', use)) counts.admitted_uses_order_verified += 1;
        check(allDependencies.some(dependency => sameSource(dependency?.source, use) &&
          dependency.resolution?.kind === 'binding' && dependency.resolution.binding_id === definition.binding_id &&
          sameSource(dependency.resolution.declaration, definition.declaration)), 'use_dependency_missing', use);
      }
      check(isDeepStrictEqual(definition.initializer, definition.expression.nodes[definition.expression.root].expression),
        'initializer_root', definition.initializer.source);
      for (const entry of definition.expression.nodes) {
        check(arenaShape(entry, definition, rule, file), 'arena_shape', entry.expression.source);
        if (entry.kind?.kind === 'unsupported') {
          gapLink(entry.kind.gap_id, fact.id, rule, entry.expression.source);
        }
      }
      for (const dependency of definition.dependencies) {
        counts.initializer_dependencies += 1;
        if (dependency.resolution.kind === 'unresolved') {
          gapLink(dependency.resolution.gap_id, fact.id, rule, dependency.source);
        }
        if (dependency.resolution.kind !== 'binding') continue;
        const read = ast(dependency.source, ts.isIdentifier);
        const target = ast(dependency.resolution.declaration);
        const symbol = read && checker.getSymbolAtLocation(read);
        function matchesDeclaredBinding(node) {
          if (sameSpan(span(file, node), dependency.resolution.declaration)) return true;
          // Tree-sitter cites a destructuring declaration/parameter as a whole;
          // TypeScript's symbol points to its nested BindingElement instead.
          if (!ts.isBindingElement(node)) return false;
          while (node.parent && (ts.isBindingElement(node) || ts.isObjectBindingPattern(node) || ts.isArrayBindingPattern(node))) {
            node = node.parent;
            if ((ts.isParameter(node) || ts.isVariableDeclaration(node)) &&
              sameSpan(span(file, node), dependency.resolution.declaration)) return true;
          }
          return false;
        }
        check(!!target && !!symbol?.declarations?.some(matchesDeclaredBinding), 'dependency_symbol', dependency.source);
      }
    }
  }
  return {
    status: issueCount === 0 ? 'source_syntax_binding_checks_passed' : 'source_syntax_binding_mismatches',
    scope: 'Source-only syntax and lexical binding; no runtime, complete input closure, secrecy, or captured-source receipt proof.',
    typescript_version: ts.version,
    staged_source_files: sourceFiles.size,
    staged_source_bytes: totalBytes,
    counts,
    issue_count: issueCount,
    diagnostics_truncated: issueCount > issues.length,
    issues,
  };
}

function main() {
  const args = process.argv.slice(2);
  requireInput(args.length === 3 || args.length === 4, 'usage_source_root_manifest_output_directory_optional_audit_json');
  const sourceRoot = fs.realpathSync(args[0]);
  requireInput(fs.statSync(sourceRoot).isDirectory(), 'invalid_source_root');
  const manifestPath = path.resolve(args[1]);
  const graphPath = path.join(path.resolve(args[2]), 'graph.json');
  let destination;
  if (args[3]) {
    const parent = fs.realpathSync(path.dirname(path.resolve(args[3])));
    destination = path.join(parent, path.basename(args[3]));
    const relative = path.relative(sourceRoot, destination);
    requireInput(relative === '..' || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative), 'audit_output_inside_source_root');
    requireInput(destination !== manifestPath && destination !== graphPath, 'audit_output_overlaps_input');
  }
  // Resolve only this checkout's installed UI dependency. No search/install or
  // fallback to another project, global package, network or target dependency.
  const ts = require(path.join(__dirname, '..', 'ui', 'node_modules', 'typescript', 'lib', 'typescript.js'));
  requireInput(ts.version === EXPECTED_TYPESCRIPT_VERSION, 'installed_typescript_version_mismatch');
  const result = createAudit(ts, sourceRoot, readJson(manifestPath, LIMITS.manifestBytes), readJson(graphPath, LIMITS.graphBytes));
  const json = `${JSON.stringify(result, null, 2)}\n`;
  // Exclusive creation prevents replacing existing input or following a symlink.
  if (destination) fs.writeFileSync(destination, json, { flag: 'wx', mode: 0o600 });
  process.stdout.write(json);
  process.exitCode = result.issue_count === 0 ? 0 : 1;
}

try {
  main();
} catch (error) {
  // Never print third-party exception messages: parser and filesystem errors can
  // contain source text or arbitrary paths. All diagnostics have fixed reasons.
  const failure = error instanceof AuditFailure ? error : new AuditFailure('audit_input_dependency_or_parser_failure');
  process.stdout.write(`${JSON.stringify({
    status: 'audit_not_completed',
    expected_typescript_version: EXPECTED_TYPESCRIPT_VERSION,
    issues: [{ reason: failure.reason, citation: failure.citation }],
  }, null, 2)}\n`);
  process.exitCode = 2;
}
