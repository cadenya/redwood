// Shared provisioning for language conformance environments: isolated,
// input-keyed caches under the OS temp directory.
//
// Keys hash the PACKAGED ARTIFACT CONTENTS (every source/data file that
// ships) plus interpreter identity — metadata-only keys would keep serving
// a stale installed SDK after ordinary generator changes. Provisioning
// builds into a scratch directory that is renamed into place only after an
// import probe succeeds, so a failed install can never masquerade as a
// ready environment. Single-process ownership is assumed (the conformance
// runners are not run concurrently against one cache).

import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import {
  existsSync,
  realpathSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const CACHE_ROOT = join(tmpdir(), 'redwood-conformance');

function hashTree(hash, dir, prefix = '') {
  for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) =>
    a.name.localeCompare(b.name),
  )) {
    const path = join(dir, entry.name);
    const rel = `${prefix}${entry.name}`;
    if (entry.isDirectory()) {
      hashTree(hash, path, `${rel}/`);
    } else if (entry.isFile()) {
      hash.update(rel);
      hash.update(readFileSync(path));
    }
  }
}

function cacheKey(parts, trees) {
  const hash = createHash('sha256');
  // Length-prefixed framing so adjacent parts can't collide across the
  // boundary -- no delimiter byte needed.
  for (const part of parts) {
    const bytes = Buffer.from(`${part}`, 'utf8');
    hash.update(`${bytes.length}:`);
    hash.update(bytes);
  }
  for (const tree of trees) hashTree(hash, tree);
  return hash.digest('hex').slice(0, 16);
}

function run(cmd, args, opts = {}) {
  return execFileSync(cmd, args, { encoding: 'utf8', timeout: 300_000, ...opts });
}

/**
 * Provision into `<dir>.building`, verify with `probe(buildingDir)`, then
 * atomically rename into the cache. Returns the final dir.
 */
function provisioned(dir, build, probe) {
  if (existsSync(join(dir, '.redwood-ready'))) return dir;
  const scratch = `${dir}.building`;
  rmSync(scratch, { recursive: true, force: true });
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(scratch, { recursive: true });
  build(scratch);
  probe(scratch);
  writeFileSync(join(scratch, '.redwood-ready'), 'ok');
  renameSync(scratch, dir);
  return dir;
}

/**
 * A venv created from the selected base interpreter with gen/python
 * installed FROM ITS OWN pyproject.toml. The import probe proves the
 * INSTALLED distribution loads (from the venv, not the source tree).
 * Returns the venv's python path.
 */
export function provisionPython(pyDir) {
  const base = process.env.PYTHON ?? 'python3';
  const baseVersion = run(base, ['--version']).trim();
  const pyproject = readFileSync(join(pyDir, 'pyproject.toml'), 'utf8');
  const dir = join(
    CACHE_ROOT,
    `venv-${cacheKey([base, baseVersion, pyproject], [join(pyDir, 'cadenya')])}`,
  );
  provisioned(
    dir,
    (scratch) => {
      console.log(`provisioning python env (${baseVersion})...`);
      run(base, ['-m', 'venv', scratch]);
      run(join(scratch, 'bin', 'python'), ['-m', 'pip', 'install', '--quiet', pyDir]);
    },
    (scratch) => {
      const loaded = run(join(scratch, 'bin', 'python'), [
        '-c',
        'import cadenya, sys; print(cadenya.__file__)',
      ]).trim();
      // realpath both sides: macOS tmpdir is a symlink (/var -> /private/var).
      if (!realpathSync(loaded).startsWith(realpathSync(scratch))) {
        throw new Error(`installed-package probe loaded ${loaded}, not the venv install`);
      }
    },
  );
  return join(dir, 'bin', 'python');
}

/**
 * An isolated GEM_HOME with the generated gem built and installed BY THE
 * SELECTED RUBY's own RubyGems. The require probe proves the INSTALLED gem
 * loads from the gem home, not the source tree.
 * Returns { ruby, gemHome }.
 */
export function provisionRuby(rubyDir) {
  const ruby = process.env.RUBY ?? 'ruby';
  const rubyVersion = run(ruby, ['--version']).trim();
  const gemspec = readFileSync(join(rubyDir, 'cadenya.gemspec'), 'utf8');
  const gemHome = join(
    CACHE_ROOT,
    `gem-home-${cacheKey([ruby, rubyVersion, gemspec], [join(rubyDir, 'lib')])}`,
  );
  provisioned(
    gemHome,
    (scratch) => {
      console.log(`provisioning ruby gem home (${rubyVersion})...`);
      const gemFile = join(scratch, 'cadenya-conformance.gem');
      run(ruby, ['-S', 'gem', 'build', 'cadenya.gemspec', '--output', gemFile], { cwd: rubyDir });
      run(ruby, ['-S', 'gem', 'install', '--install-dir', scratch, '--no-document', gemFile]);
    },
    (scratch) => {
      const loaded = run(
        ruby,
        ['-e', 'require "cadenya"; puts $LOADED_FEATURES.grep(/cadenya\\/client/).first'],
        { env: { ...process.env, GEM_HOME: scratch, GEM_PATH: scratch }, cwd: CACHE_ROOT },
      ).trim();
      if (!realpathSync(loaded).startsWith(realpathSync(scratch))) {
        throw new Error(`installed-gem probe loaded ${loaded}, not the gem home`);
      }
    },
  );
  return { ruby, gemHome };
}
