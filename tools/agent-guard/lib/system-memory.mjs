// What the machine actually has right now — not what it had at boot, and not
// what a constant in a config file claims.
//
// The guard this replaces sized its ceilings against nothing: a hardcoded
// 4096 MB default and an 8192 MB e2e override, both of which are unreachable
// on an 8 GB machine, so the ceiling could never trip on the one class of
// machine that needed it. Every number this tool uses is derived here instead.
//
// Two quantities matter and they are not the same:
//   - `availableMb` — memory a new process can take without evicting something
//     that will immediately be faulted back in.
//   - swap usage — the signal that the machine has ALREADY lost this argument.
//     A box with 1.5 GB "available" and 6 GB of swap committed is not healthy;
//     it is thrashing, and starting three Electron workers is what turns
//     thrashing into a force-reboot.
//
// Parsers are pure and exported so the platform arithmetic is testable without
// a machine in the required state; only `readMemoryStatus` touches the system.

import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import os from 'node:os';

const MB = 1024 * 1024;

function toMb(bytes) {
  return Math.round(bytes / MB);
}

/**
 * `vm_stat` on macOS. Page counts, with the page size in the header line.
 *
 * `inactive` counts as available: on macOS it is predominantly reclaimable
 * file cache, and excluding it would report ~100 MB available on a perfectly
 * healthy laptop and refuse every run. The correction for the case where
 * inactive pages are in fact dirty is the swap signal, which is checked
 * independently — a machine whose inactive pages are not really free is a
 * machine that is already swapping, and that is caught by `swapUsedRatio`.
 */
export function parseVmStat(output) {
  const pageSizeMatch = /page size of (\d+) bytes/u.exec(output);
  const pageSize = pageSizeMatch ? Number(pageSizeMatch[1]) : 4096;
  const pages = (label) => {
    const match = new RegExp(`^${label}:\\s+(\\d+)\\.?\\s*$`, 'mu').exec(output);
    return match ? Number(match[1]) : 0;
  };
  const free = pages('Pages free');
  const speculative = pages('Pages speculative');
  const inactive = pages('Pages inactive');
  const purgeable = pages('Pages purgeable');
  const compressed = pages('Pages occupied by compressor') || pages('Pages occupying compressor');
  return {
    pageSize,
    availableMb: toMb((free + speculative + inactive + purgeable) * pageSize),
    compressedMb: toMb(compressed * pageSize),
  };
}

/** `sysctl vm.swapusage` → `total = 7168.00M  used = 6090.00M  free = 1078.00M`. */
export function parseSwapusage(output) {
  const field = (name) => {
    const match = new RegExp(`${name}\\s*=\\s*([\\d.]+)([KMG])`, 'u').exec(output);
    if (!match) return 0;
    const value = Number(match[1]);
    const scale = match[2] === 'G' ? 1024 : match[2] === 'K' ? 1 / 1024 : 1;
    return Math.round(value * scale);
  };
  return { swapTotalMb: field('total'), swapUsedMb: field('used') };
}

/** `/proc/meminfo` on Linux. `MemAvailable` is the kernel's own estimate. */
export function parseMeminfo(output) {
  const kb = (label) => {
    const match = new RegExp(`^${label}:\\s+(\\d+) kB`, 'mu').exec(output);
    return match ? Number(match[1]) : 0;
  };
  const swapTotalKb = kb('SwapTotal');
  const swapFreeKb = kb('SwapFree');
  return {
    availableMb: Math.round(kb('MemAvailable') / 1024),
    swapTotalMb: Math.round(swapTotalKb / 1024),
    swapUsedMb: Math.round((swapTotalKb - swapFreeKb) / 1024),
  };
}

function cgroupBases(readFile) {
  const bases = new Set(['/sys/fs/cgroup']);
  try {
    for (const line of readFile('/proc/self/cgroup', 'utf8').split('\n')) {
      const [hierarchy, controllers, relative = ''] = line.split(':');
      if (hierarchy === '0' && controllers === '') bases.add(`/sys/fs/cgroup${relative}`);
      if (controllers?.split(',').includes('memory')) bases.add(`/sys/fs/cgroup/memory${relative}`);
    }
  } catch {
    // Root candidates still cover the common container layouts.
  }
  return [...bases];
}

/** The finite memory limit and current usage of a cgroup v1 or v2 process. */
export function readCgroupMemory(readFile = readFileSync) {
  const candidates = [];
  for (const base of cgroupBases(readFile)) {
    candidates.push([`${base}/memory.max`, `${base}/memory.current`]);
    candidates.push([`${base}/memory.limit_in_bytes`, `${base}/memory.usage_in_bytes`]);
  }
  let effective = null;
  for (const [limitPath, currentPath] of candidates) {
    try {
      const rawLimit = readFile(limitPath, 'utf8').trim();
      if (rawLimit === 'max') continue;
      const limitBytes = Number(rawLimit);
      const currentBytes = Number(readFile(currentPath, 'utf8').trim());
      if (!Number.isSafeInteger(limitBytes) || limitBytes <= 0 || !Number.isSafeInteger(currentBytes) || currentBytes < 0) continue;
      const availableBytes = Math.max(0, limitBytes - currentBytes);
      effective = {
        limitBytes: Math.min(effective?.limitBytes ?? Number.POSITIVE_INFINITY, limitBytes),
        availableBytes: Math.min(effective?.availableBytes ?? Number.POSITIVE_INFINITY, availableBytes),
      };
    } catch {
      // Try the next supported cgroup layout.
    }
  }
  return effective;
}

function run(command, args) {
  return execFileSync(command, args, { encoding: 'utf8', timeout: 5000, maxBuffer: 4 * MB });
}

/**
 * Live memory status, or a `degraded` reading when the platform tools are
 * unavailable.
 *
 * Degradation is explicit rather than silent: `os.freemem()` alone badly
 * overstates pressure on macOS and says nothing at all about swap, so a
 * fallback reading is marked and the admission logic treats it as
 * unverified rather than as good news.
 */
export function readMemoryStatus({ platform = process.platform, totalMb = toMb(os.totalmem()), exec = run, readFile = readFileSync } = {}) {
  try {
    if (platform === 'darwin') {
      const { availableMb, compressedMb } = parseVmStat(exec('vm_stat', []));
      const { swapTotalMb, swapUsedMb } = parseSwapusage(exec('sysctl', ['vm.swapusage']));
      return { totalMb, availableMb, compressedMb, swapTotalMb, swapUsedMb, source: 'vm_stat+sysctl', degraded: false };
    }
    if (platform === 'linux') {
      const { availableMb, swapTotalMb, swapUsedMb } = parseMeminfo(readFile('/proc/meminfo', 'utf8'));
      const cgroup = readCgroupMemory(readFile);
      if (cgroup) {
        const cgroupTotalMb = Math.floor(cgroup.limitBytes / MB);
        const cgroupAvailableMb = Math.floor(cgroup.availableBytes / MB);
        return {
          totalMb: Math.min(totalMb, cgroupTotalMb),
          availableMb: Math.min(availableMb, cgroupAvailableMb),
          compressedMb: 0,
          swapTotalMb,
          swapUsedMb,
          source: '/proc/meminfo+cgroup',
          degraded: false,
        };
      }
      return { totalMb, availableMb, compressedMb: 0, swapTotalMb, swapUsedMb, source: '/proc/meminfo', degraded: false };
    }
  } catch {
    // Fall through to the degraded reading below.
  }
  return {
    totalMb,
    availableMb: toMb(os.freemem()),
    compressedMb: 0,
    swapTotalMb: 0,
    swapUsedMb: 0,
    source: 'os.freemem',
    degraded: true,
  };
}

/**
 * The largest resident processes, so a refusal can name what is holding the
 * memory instead of telling the user to go find out.
 */
export function topConsumers(limit = 5, exec = run) {
  try {
    const rows = exec('ps', ['-axo', 'rss=,comm='])
      .split('\n')
      .map((line) => /^\s*(\d+)\s+(.*)$/u.exec(line.trimEnd()))
      .filter(Boolean)
      .map((match) => ({ rssMb: Math.round(Number(match[1]) / 1024), name: match[2].split('/').pop() }));
    const byName = new Map();
    for (const { rssMb, name } of rows) byName.set(name, (byName.get(name) ?? 0) + rssMb);
    return [...byName]
      .map(([name, rssMb]) => ({ name, rssMb }))
      .sort((a, b) => b.rssMb - a.rssMb)
      .slice(0, limit);
  } catch {
    return [];
  }
}
