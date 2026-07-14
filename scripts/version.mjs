#!/usr/bin/env node

import { existsSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const SCRIPT_PATH = fileURLToPath(import.meta.url);
const DEFAULT_ROOT = path.resolve(path.dirname(SCRIPT_PATH), '..');

const readText = (root, relativePath) => readFileSync(path.join(root, relativePath), 'utf8');
const readJson = (root, relativePath) => JSON.parse(readText(root, relativePath));
const jsonText = (value) => `${JSON.stringify(value, null, 2)}\n`;

export function assertSemver(version) {
  const match = version.match(
    /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/u,
  );
  if (!match) throw new Error(`Invalid semantic version: ${version}`);
  for (const identifier of match[4]?.split('.') ?? []) {
    if (/^\d+$/u.test(identifier) && identifier.length > 1 && identifier.startsWith('0')) {
      throw new Error(`Invalid semantic version: ${version}`);
    }
  }
}

function sectionRange(source, name) {
  const header = `[${name}]`;
  const start = source.indexOf(header);
  if (start < 0) throw new Error(`Missing ${header} section`);
  const contentStart = start + header.length;
  const nextOffset = source.slice(contentStart).search(/^\[/mu);
  const end = nextOffset < 0 ? source.length : contentStart + nextOffset;
  return { start, end };
}

function sectionVersion(source, name) {
  const { start, end } = sectionRange(source, name);
  const match = source.slice(start, end).match(/^version\s*=\s*"([^"]+)"/mu);
  if (!match) throw new Error(`Missing version in [${name}]`);
  return match[1];
}

function replaceSectionVersion(source, name, version) {
  const { start, end } = sectionRange(source, name);
  const section = source.slice(start, end);
  if (!/^version\s*=\s*"[^"]+"/mu.test(section)) {
    throw new Error(`Missing version in [${name}]`);
  }
  return (
    source.slice(0, start) +
    section.replace(/^version\s*=\s*"[^"]+"/mu, `version = "${version}"`) +
    source.slice(end)
  );
}

function workspacePackageNames(root) {
  const manifests = ['src-tauri/Cargo.toml'];
  const cratesDir = path.join(root, 'crates');
  for (const entry of readdirSync(cratesDir, { withFileTypes: true })
    .filter((item) => item.isDirectory())
    .sort((left, right) => left.name.localeCompare(right.name))) {
    const relativePath = `crates/${entry.name}/Cargo.toml`;
    if (existsSync(path.join(root, relativePath))) manifests.push(relativePath);
  }

  return manifests.map((relativePath) => {
    const source = readText(root, relativePath);
    const { start, end } = sectionRange(source, 'package');
    const section = source.slice(start, end);
    const name = section.match(/^name\s*=\s*"([^"]+)"/mu)?.[1];
    if (!name) throw new Error(`Missing package name in ${relativePath}`);
    if (!/^version\.workspace\s*=\s*true\s*$/mu.test(section)) {
      throw new Error(`${relativePath} must inherit version.workspace = true`);
    }
    return name;
  });
}

function cargoLockVersions(source, packageNames) {
  const wanted = new Set(packageNames);
  const versions = new Map();
  for (const block of source.split(/(?=^\[\[package\]\]$)/mu)) {
    const name = block.match(/^name\s*=\s*"([^"]+)"/mu)?.[1];
    if (!name || !wanted.has(name)) continue;
    const version = block.match(/^version\s*=\s*"([^"]+)"/mu)?.[1];
    if (!version) throw new Error(`Missing Cargo.lock version for ${name}`);
    versions.set(name, version);
  }
  return versions;
}

function rewriteCargoLock(source, packageNames, version) {
  const wanted = new Set(packageNames);
  const seen = new Set();
  const rewritten = source
    .split(/(?=^\[\[package\]\]$)/mu)
    .map((block) => {
      const name = block.match(/^name\s*=\s*"([^"]+)"/mu)?.[1];
      if (!name || !wanted.has(name)) return block;
      if (!/^version\s*=\s*"[^"]+"/mu.test(block)) {
        throw new Error(`Missing Cargo.lock version for ${name}`);
      }
      seen.add(name);
      return block.replace(/^version\s*=\s*"[^"]+"/mu, `version = "${version}"`);
    })
    .join('');

  const missing = packageNames.filter((name) => !seen.has(name));
  if (missing.length > 0) {
    throw new Error(`Cargo.lock is missing workspace packages: ${missing.join(', ')}`);
  }
  return rewritten;
}

function versionSources(root) {
  const rootPackage = readJson(root, 'package.json');
  const rootLock = readJson(root, 'package-lock.json');
  const uiPackage = readJson(root, 'ui/package.json');
  const uiLock = readJson(root, 'ui/package-lock.json');
  const cargoToml = readText(root, 'Cargo.toml');
  const tauri = readJson(root, 'src-tauri/tauri.conf.json');
  const cargoLock = readText(root, 'Cargo.lock');
  const packageNames = workspacePackageNames(root);
  const lockedPackages = cargoLockVersions(cargoLock, packageNames);

  const sources = new Map([
    ['package.json', rootPackage.version],
    ['package-lock.json', rootLock.version],
    ['package-lock.json packages[""]', rootLock.packages?.['']?.version],
    ['ui/package.json', uiPackage.version],
    ['ui/package-lock.json', uiLock.version],
    ['ui/package-lock.json packages[""]', uiLock.packages?.['']?.version],
    ['Cargo.toml [workspace.package]', sectionVersion(cargoToml, 'workspace.package')],
    ['src-tauri/tauri.conf.json', tauri.version],
  ]);
  for (const name of packageNames) {
    sources.set(`Cargo.lock package ${name}`, lockedPackages.get(name));
  }
  return { canonical: rootPackage.version, sources };
}

export function checkVersions(root = DEFAULT_ROOT) {
  const { canonical, sources } = versionSources(root);
  assertSemver(canonical);
  const drift = [...sources].filter(([, version]) => version !== canonical);
  if (drift.length > 0) {
    const details = drift
      .map(([source, version]) => `- ${source}: ${version ?? '<missing>'} (expected ${canonical})`)
      .join('\n');
    throw new Error(`Version sources are out of sync:\n${details}`);
  }
  return canonical;
}

export function syncVersions(root = DEFAULT_ROOT, requestedVersion) {
  const rootPackage = readJson(root, 'package.json');
  const version = requestedVersion ?? rootPackage.version;
  assertSemver(version);

  const rootLock = readJson(root, 'package-lock.json');
  const uiPackage = readJson(root, 'ui/package.json');
  const uiLock = readJson(root, 'ui/package-lock.json');
  const tauri = readJson(root, 'src-tauri/tauri.conf.json');
  const cargoToml = readText(root, 'Cargo.toml');
  const cargoLock = readText(root, 'Cargo.lock');
  const packageNames = workspacePackageNames(root);

  rootPackage.version = version;
  rootLock.version = version;
  if (!rootLock.packages?.['']) throw new Error('package-lock.json has no root package entry');
  rootLock.packages[''].version = version;
  uiPackage.version = version;
  uiLock.version = version;
  if (!uiLock.packages?.['']) throw new Error('ui/package-lock.json has no root package entry');
  uiLock.packages[''].version = version;
  tauri.version = version;

  const outputs = new Map([
    ['package.json', jsonText(rootPackage)],
    ['package-lock.json', jsonText(rootLock)],
    ['ui/package.json', jsonText(uiPackage)],
    ['ui/package-lock.json', jsonText(uiLock)],
    ['Cargo.toml', replaceSectionVersion(cargoToml, 'workspace.package', version)],
    ['Cargo.lock', rewriteCargoLock(cargoLock, packageNames, version)],
    ['src-tauri/tauri.conf.json', jsonText(tauri)],
  ]);
  for (const [relativePath, contents] of outputs) {
    writeFileSync(path.join(root, relativePath), contents);
  }
  return checkVersions(root);
}

function runCli() {
  const [command, version, ...extra] = process.argv.slice(2);
  if (extra.length > 0 || !['check', 'sync'].includes(command)) {
    throw new Error('Usage: node scripts/version.mjs <check|sync> [version]');
  }
  if (command === 'check' && version) {
    throw new Error('The check command does not accept a version');
  }
  const synchronized = command === 'check' ? checkVersions() : syncVersions(DEFAULT_ROOT, version);
  console.log(`Version sources are synchronized at ${synchronized}.`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === SCRIPT_PATH) {
  try {
    runCli();
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}
