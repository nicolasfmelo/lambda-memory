import { closeSync, fsyncSync, lstatSync, openSync, readFileSync, renameSync, unlinkSync, writeSync } from 'node:fs';
import { isAbsolute, join } from 'node:path';

export const REGISTRY_FILE = 'projects.json';
export const MAX_REGISTRY_BYTES = 256 * 1024;
export const MAX_REGISTRY_ENTRIES = 256;
export const DEBOUNCE_MS = 60000;

function validEntry(entry) {
  return entry && typeof entry === 'object' && !Array.isArray(entry)
    && typeof entry.project === 'string' && entry.project.length > 0 && Buffer.byteLength(entry.project) <= 512
    && typeof entry.cwd === 'string' && isAbsolute(entry.cwd) && !/[\u0000-\u001f\u007f]/.test(entry.cwd) && Buffer.byteLength(entry.cwd) <= 4096
    && Number.isSafeInteger(entry.last_seen_ms) && entry.last_seen_ms >= 0;
}

/** Returns [{project, cwd, last_seen_ms}]; a missing file is an empty registry, anything unsafe throws. */
export function loadRegistry(dataDir) {
  const path = join(dataDir, REGISTRY_FILE);
  let stat;
  try { stat = lstatSync(path); } catch (failure) { if (failure.code === 'ENOENT') return []; throw failure; }
  if (!stat.isFile() || stat.uid !== process.getuid() || (stat.mode & 0o777) !== 0o600 || stat.size > MAX_REGISTRY_BYTES) throw new Error('private project registry required');
  const data = JSON.parse(readFileSync(path, 'utf8'));
  if (!data || typeof data !== 'object' || Array.isArray(data) || !Array.isArray(data.projects)
      || data.projects.length > MAX_REGISTRY_ENTRIES || !data.projects.every(validEntry)) throw new Error('invalid project registry');
  return data.projects.map(({ project, cwd, last_seen_ms }) => ({ project, cwd, last_seen_ms }));
}

const queues = new Map(); // dataDir -> promise tail
const lastWrite = new Map(); // dataDir\0project -> { cwd, at }

function writeAtomic(dataDir, entries) {
  const path = join(dataDir, REGISTRY_FILE), temp = `${path}.${process.pid}.tmp`;
  try { unlinkSync(temp); } catch (failure) { if (failure.code !== 'ENOENT') throw failure; }
  const fd = openSync(temp, 'wx', 0o600);
  try {
    try { writeSync(fd, JSON.stringify({ projects: entries })); fsyncSync(fd); } finally { closeSync(fd); }
    renameSync(temp, path);
  } catch (failure) { try { unlinkSync(temp); } catch {} throw failure; }
}

/** Records that a project was seen. Writes are serialized, atomic and debounced per project+cwd. */
export function recordProject(dataDir, { project, cwd }, nowMs = Date.now()) {
  if (!validEntry({ project, cwd, last_seen_ms: nowMs })) return Promise.resolve(false);
  const key = `${dataDir}\0${project}`, previous = lastWrite.get(key);
  if (previous && previous.cwd === cwd && nowMs - previous.at < DEBOUNCE_MS) return Promise.resolve(false);
  lastWrite.set(key, { cwd, at: nowMs });
  const run = async () => {
    let entries;
    try { entries = loadRegistry(dataDir); }
    catch (failure) {
      // Only an untrusted or corrupt file is rebuilt; transient I/O errors skip the write.
      if (failure instanceof SyntaxError || failure.message === 'private project registry required' || failure.message === 'invalid project registry') entries = [];
      else throw failure;
    }
    entries = entries.filter(entry => entry.project !== project);
    entries.push({ project, cwd, last_seen_ms: nowMs });
    entries.sort((a, b) => b.last_seen_ms - a.last_seen_ms);
    writeAtomic(dataDir, entries.slice(0, MAX_REGISTRY_ENTRIES));
    return true;
  };
  const tail = (queues.get(dataDir) ?? Promise.resolve()).catch(() => {}).then(run);
  queues.set(dataDir, tail);
  tail.catch(() => lastWrite.delete(key));
  return tail;
}
