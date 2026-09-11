// MT-H4-02 runner setup only. Actual inference uses the production Rust worker.
// This driver never constructs a model response, reads the oracle, or retries a task.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { mkdir, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';

export const PIN = Object.freeze({
  runtimeVersion: '0.34.0',
  runtimeUrl: 'https://github.com/ollama/ollama/releases/download/v0.34.0/ollama-linux-amd64.tar.zst',
  runtimeSha256: 'cf95886728959aa09910bb34de5cca1cc5a8f68003b5597197d3f2c2d57c0804',
  runtimeBytes: 1433537033,
  model: 'qwen3:8b',
  modelDigest: '500a1f067a9f782620b40bee6f7b0c89e17ae61f686b92c24933e4ca4b2b8b41',
  contextLength: 8192,
});
const endpoint = 'http://127.0.0.1:11435';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

export function verifyInventory(inventory, pin = PIN) {
  const models = inventory.models?.filter((model) => model.name === pin.model);
  assert.equal(models?.length, 1, 'Expected exactly the pinned installed model');
  assert.equal(models[0].digest?.replace(/^sha256:/u, ''), pin.modelDigest,
    'The installed model manifest differs from the reviewed pin');
  return models[0];
}

function command(executable, args, { env = process.env, timeout = 600_000, quiet = false } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(executable, args, {
      cwd: root, env, stdio: quiet ? 'ignore' : 'inherit', timeout,
    });
    child.once('error', reject);
    child.once('exit', (code, signal) => {
      if (code === 0) resolve();
      else reject(new Error(`${path.basename(executable)} failed (exit=${code}, signal=${signal})`));
    });
  });
}

async function localJson(route) {
  const response = await fetch(`${endpoint}${route}`, { signal: AbortSignal.timeout(3000) });
  assert.ok(response.ok, `Runtime metadata returned HTTP ${response.status}`);
  const chunks = [];
  let bytes = 0;
  for await (const chunk of response.body) {
    bytes += chunk.length;
    assert.ok(bytes <= 65536, 'Runtime metadata exceeds its bound');
    chunks.push(chunk);
  }
  return JSON.parse(Buffer.concat(chunks).toString('utf8'));
}

async function archiveHash(archive) {
  const hash = createHash('sha256');
  let bytes = 0;
  for await (const chunk of createReadStream(archive)) {
    bytes += chunk.length;
    assert.ok(bytes <= PIN.runtimeBytes, 'Runtime archive exceeds its reviewed size');
    hash.update(chunk);
  }
  assert.equal(bytes, PIN.runtimeBytes, 'Runtime archive is incomplete');
  return hash.digest('hex');
}

async function stop(server) {
  if (!server || server.exitCode !== null || server.signalCode !== null) return;
  server.kill('SIGTERM');
  for (let attempt = 0; attempt < 20; attempt += 1) {
    if (server.exitCode !== null || server.signalCode !== null) return;
    await delay(100);
  }
  server.kill('SIGKILL');
}

async function main() {
  assert.equal(process.env.GITHUB_ACTIONS, 'true', 'Use the governed GitHub runner, not a local agent shell');
  assert.equal(process.env.RUNNER_ENVIRONMENT, 'github-hosted', 'Only an ephemeral GitHub-hosted runner is supported');
  assert.equal(process.platform, 'linux');
  assert.equal(process.arch, 'x64');
  assert.ok(process.env.RUNNER_TEMP && path.isAbsolute(process.env.RUNNER_TEMP));
  const directory = path.join(process.env.RUNNER_TEMP, 'cartograph-local-acceptance');
  await mkdir(directory); // Never overwrite evidence or reuse a previous model task.
  const evidence = path.join(directory, 'evidence');
  const runtime = path.join(directory, 'runtime');
  await mkdir(evidence);
  await mkdir(runtime);
  const setup = {
    schema_version: 1, source_revision: process.env.GITHUB_SHA,
    pins: PIN, phase: 'runtime_download', observed_runtime: null, observed_model: null,
    hardware: { platform: process.platform, arch: process.arch, cpus: os.cpus().length,
      cpu: os.cpus()[0]?.model, memory_bytes: os.totalmem() },
    cloud_features_disabled: true, local_endpoint: endpoint,
    independent_citation_review: 'pending', delivery_gate_pass: false,
  };
  const save = () => writeFile(path.join(evidence, 'setup.json'), JSON.stringify(setup, null, 2));
  await save();
  let server;
  let serverError;
  try {
    const archive = path.join(runtime, 'ollama.tar.zst');
    await command('curl', ['--fail', '--silent', '--show-error', '--location', '--proto', '=https',
      '--connect-timeout', '10', '--max-time', '300', '--max-filesize', String(PIN.runtimeBytes),
      '--output', archive, PIN.runtimeUrl], { timeout: 310_000 });
    assert.equal(await archiveHash(archive), PIN.runtimeSha256, 'Runtime archive digest differs from the reviewed pin');
    // The hosted runner has no GPU. Exclude GPU-only libraries from disk without
    // modifying the verified CPU executable or its runtime libraries.
    await command('tar', ['--zstd', '-xf', archive, '-C', runtime,
      '--exclude=lib/ollama/cuda*', '--exclude=./lib/ollama/cuda*'], { timeout: 120_000 });
    await rm(archive);
    const executable = path.join(runtime, 'bin', 'ollama');
    const runtimeEnv = {
      ...process.env, OLLAMA_HOST: '127.0.0.1:11435', OLLAMA_MODELS: path.join(runtime, 'models'),
      OLLAMA_NO_CLOUD: '1', OLLAMA_NOHISTORY: '1', OLLAMA_NUM_PARALLEL: '1',
      OLLAMA_MAX_LOADED_MODELS: '1', OLLAMA_CONTEXT_LENGTH: String(PIN.contextLength),
    };
    setup.phase = 'runtime_start';
    await save();
    server = spawn(executable, ['serve'], { cwd: runtime, env: runtimeEnv, stdio: 'ignore' });
    server.once('error', (error) => { serverError = error; });
    const deadline = Date.now() + 60_000;
    while (!setup.observed_runtime && Date.now() < deadline) {
      assert.ok(!serverError && server.exitCode === null && server.signalCode === null, 'The isolated runtime exited');
      try { setup.observed_runtime = await localJson('/api/version'); } catch { await delay(500); }
    }
    assert.equal(setup.observed_runtime?.version, PIN.runtimeVersion, 'The running runtime version differs from the pin');
    setup.phase = 'model_download';
    await save();
    await command(executable, ['pull', PIN.model], { env: runtimeEnv, timeout: 600_000, quiet: true });
    setup.observed_model = verifyInventory(await localJson('/api/tags'));
    assert.ok(!serverError && server.exitCode === null && server.signalCode === null, 'The isolated runtime exited');
    setup.phase = 'investigation_running';
    await save();
    await command('cargo', ['test', '--locked', '-p', 'app', 'investigation_local_provider_acceptance',
      '--', '--ignored', '--nocapture'], {
      timeout: 1_260_000,
      env: {
        ...runtimeEnv, CARTOGRAPH_ACCEPTANCE_URL: `${endpoint}/`, CARTOGRAPH_ACCEPTANCE_MODEL: PIN.model,
        CARTOGRAPH_ACCEPTANCE_MODEL_DIGEST: PIN.modelDigest,
        CARTOGRAPH_ACCEPTANCE_RUNTIME: JSON.stringify({ version: setup.observed_runtime.version,
          hardware: setup.hardware, context_length: PIN.contextLength }),
        CARTOGRAPH_ACCEPTANCE_OUTPUT: path.join(evidence, 'run'),
      },
    });
    setup.phase = 'mechanical_checks_passed_review_pending';
  } catch (error) {
    setup.phase = 'failed';
    // Error messages describe process/status/identity checks, never raw model
    // output. The production coordinator owns admitted result and failure detail.
    setup.error = error.message;
    process.exitCode = 1;
  } finally {
    await stop(server);
    await save();
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch((error) => { console.error(error.message); process.exitCode = 1; });
}
