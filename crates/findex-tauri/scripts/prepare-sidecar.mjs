import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const crateRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const workspaceRoot = resolve(crateRoot, '..', '..');
const host = execFileSync('rustc', ['-vV'], { encoding: 'utf8' })
  .split(/\r?\n/)
  .find(line => line.startsWith('host: '))
  ?.slice(6)
  .trim();
const target = process.env.TAURI_ENV_TARGET_TRIPLE || process.env.CARGO_BUILD_TARGET || host;
if (!target) throw new Error('Could not determine the Rust target triple');

const cargoArgs = ['build', '--release', '--locked', '-p', 'findex-cli'];
if (target !== host) cargoArgs.push('--target', target);
execFileSync('cargo', cargoArgs, { cwd: workspaceRoot, stdio: 'inherit' });

const extension = target.includes('windows') ? '.exe' : '';
const targetRoot = target === host ? join(workspaceRoot, 'target') : join(workspaceRoot, 'target', target);
const source = join(targetRoot, 'release', `findex-cli${extension}`);
if (!existsSync(source)) throw new Error(`CLI build did not produce ${source}`);

const binaries = join(crateRoot, 'binaries');
mkdirSync(binaries, { recursive: true });
copyFileSync(source, join(binaries, `findex-${target}${extension}`));
copyFileSync(source, join(binaries, `findex${extension}`));
process.stdout.write(`Prepared unified CLI/TUI sidecar for ${target}\n`);
