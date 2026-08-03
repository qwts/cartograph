#!/usr/bin/env node
// The model routing registry (ENG-0151): validation and the read helper issue
// authors use instead of naming models from memory.
//
//   node tools/models/registry.mjs           # human-readable routing table
//   node tools/models/registry.mjs --json
//
// Zero-dependency (ENG-0004). No network: refreshing the registry is a separate,
// human-triggered lane, so reading it is always fast, offline and identical for
// every agent.

import process from 'node:process';
import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..');

export const TIERS = ['T1', 'T2', 'T3'];
export const VENDOR_GROUPS = ['anthropic', 'openai', 'chinese', 'ide_native'];
export const PHASES = ['plan', 'build'];
export const STATUSES = ['verified', 'seeded', 'unverified'];

// The access policy is data, not prose, so a refresh run cannot widen it by
// rewriting a sentence. Chinese models are reachable only through an IDE we
// already run; a direct vendor route is a decision, not an update.
export const CHINESE_ACCESS = ['cursor', 'devin'];

export function loadRegistry(root = ROOT) {
  return JSON.parse(readFileSync(join(root, 'governance', 'agent-models.json'), 'utf8'));
}

export function validateRegistry(registry) {
  const errors = [];
  const fail = (msg) => errors.push(msg);

  if (registry?.schema_version !== 1) fail('schema_version must be 1');
  if (!(registry?.verified_at === null || /^\d{4}-\d{2}-\d{2}$/.test(registry?.verified_at ?? ''))) {
    fail('verified_at must be null or an ISO date (YYYY-MM-DD)');
  }

  const access = registry?.policy?.chinese_models?.access;
  if (!Array.isArray(access) || access.length !== CHINESE_ACCESS.length || !CHINESE_ACCESS.every((a) => access.includes(a))) {
    fail(`policy.chinese_models.access must be exactly ${JSON.stringify(CHINESE_ACCESS)} — widening it is a human decision, not a refresh`);
  }

  for (const tier of TIERS) {
    const entry = registry?.tiers?.[tier];
    if (!entry) {
      fail(`tier ${tier} is missing`);
      continue;
    }
    if (!entry.when) fail(`tier ${tier} must say when it applies`);
    for (const vendor of VENDOR_GROUPS) {
      const v = entry.vendors?.[vendor];
      if (!v) {
        fail(`tier ${tier} is missing vendor group ${vendor}`);
        continue;
      }
      if (!Array.isArray(v.available_in)) fail(`${tier}.${vendor}.available_in must be an array`);
      // Availability is as load-bearing as the model name: a recommendation
      // naming something nobody here can invoke is worse than no recommendation.
      if (vendor === 'chinese' && Array.isArray(v.available_in)) {
        const extra = v.available_in.filter((a) => !CHINESE_ACCESS.includes(a));
        if (extra.length) fail(`${tier}.chinese.available_in may not include ${extra.join(', ')} — see policy.chinese_models`);
      }
      for (const phase of PHASES) {
        const slot = v[phase];
        if (!slot) {
          fail(`${tier}.${vendor}.${phase} is missing`);
          continue;
        }
        if (!STATUSES.includes(slot.status)) fail(`${tier}.${vendor}.${phase}.status must be one of ${STATUSES.join(', ')}`);
        // A named model with no status of 'verified'/'seeded' would read as
        // confirmed; an unverified slot must carry no name at all rather than a
        // plausible guess someone will act on.
        if (slot.status === 'unverified' && (slot.model !== null || slot.reasoning !== null)) {
          fail(`${tier}.${vendor}.${phase} is unverified and must leave model and reasoning null`);
        }
        if (slot.status !== 'unverified' && !slot.model) {
          fail(`${tier}.${vendor}.${phase} claims status ${slot.status} but names no model`);
        }
        // A named model with nowhere to run it renders as an actionable
        // recommendation followed by "via unknown". The refresh task treats
        // availability as load-bearing, so the validator has to agree: a refresh
        // that fills in a model and forgets available_in would otherwise ship a
        // route nobody can take.
        if (slot.status !== 'unverified' && v.available_in?.length === 0) {
          fail(`${tier}.${vendor}.${phase} names a model but ${tier}.${vendor}.available_in is empty — a route nobody can invoke is not a recommendation`);
        }
      }
    }
  }
  return errors;
}

// What an issue author should print. Slots that were never confirmed say so
// rather than being omitted — a gap the author can see is a gap they will
// mention; a silently dropped row reads as "no recommendation exists".
export function routingFor(registry, tier) {
  const entry = registry.tiers?.[tier];
  if (!entry) throw new Error(`unknown tier ${tier}`);
  return VENDOR_GROUPS.map((vendor) => {
    const v = entry.vendors[vendor];
    const render = (slot) =>
      slot.status === 'unverified'
        ? 'unverified — do not guess'
        : `${slot.model} (reasoning ${slot.reasoning}${slot.status === 'seeded' ? ', provisional' : ''})`;
    return {
      vendor,
      plan: render(v.plan),
      build: render(v.build),
      available_in: v.available_in.length ? v.available_in.join(', ') : 'unknown',
    };
  });
}

export function staleness(registry) {
  const seeded = [];
  const unverified = [];
  for (const tier of TIERS) {
    for (const vendor of VENDOR_GROUPS) {
      for (const phase of PHASES) {
        const status = registry.tiers?.[tier]?.vendors?.[vendor]?.[phase]?.status;
        if (status === 'seeded') seeded.push(`${tier}.${vendor}.${phase}`);
        if (status === 'unverified') unverified.push(`${tier}.${vendor}.${phase}`);
      }
    }
  }
  return { verified_at: registry.verified_at ?? null, seeded, unverified };
}

function main() {
  const registry = loadRegistry();
  const errors = validateRegistry(registry);
  if (errors.length) {
    for (const e of errors) process.stderr.write(`registry: ${e}\n`);
    process.exitCode = 1;
    return;
  }
  if (process.argv.includes('--json')) {
    process.stdout.write(`${JSON.stringify(registry, null, 2)}\n`);
    return;
  }
  const { verified_at, seeded, unverified } = staleness(registry);
  process.stdout.write(`verified_at: ${verified_at ?? 'never — no refresh run has confirmed this registry'}\n\n`);
  for (const tier of TIERS) {
    process.stdout.write(`${tier} (${registry.tiers[tier].name})\n`);
    for (const row of routingFor(registry, tier)) {
      process.stdout.write(`  ${row.vendor.padEnd(11)} plan: ${row.plan.padEnd(46)} build: ${row.build.padEnd(46)} via ${row.available_in}\n`);
    }
    process.stdout.write('\n');
  }
  if (seeded.length) process.stdout.write(`${seeded.length} slot(s) provisional (seeded by hand, never confirmed against vendor docs)\n`);
  if (unverified.length) process.stdout.write(`${unverified.length} slot(s) unverified — cite them as unknown rather than substituting a remembered model\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main();
