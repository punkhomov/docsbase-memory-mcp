import { statSync } from 'node:fs';

type Identity = { dev: bigint; ino: bigint };
type Inspection = { identity?: Identity; error?: string };

function inspect(root: string): Inspection {
  try {
    // Follow junctions/symlinks, keeping file IDs exact on every native platform.
    const stat = statSync(root, { bigint: true });
    if (!stat.isDirectory()) return { error: 'ENOTDIR' };
    if (stat.ino === 0n) return { error: 'unavailable-file-id' };
    return { identity: { dev: stat.dev, ino: stat.ino } };
  } catch (error) {
    const code = error instanceof Error && 'code' in error ? String(error.code) : 'stat-failed';
    return { error: code.slice(0, 40) };
  }
}

function describe(root: string, inspection: Inspection): string {
  const quoted = JSON.stringify(root);
  const path = quoted.length > 256 ? `${quoted.slice(0, 253)}...` : quoted;
  const identity = inspection.identity;
  return `${path} ${identity ? `dev=${identity.dev} ino=${identity.ino}` : `error=${inspection.error}`}`;
}

// Test fixture only: select the registered ID before binding the native client.
export function registeredProject<T extends { id: number; root: string }>(
  projects: readonly T[],
  root: string,
): T {
  // Rust canonical paths and Node realpath strings need not share a spelling
  // (notably Windows verbatim paths). This is fixture identity, not SDK path policy.
  const expected = inspect(root);
  const candidates = projects.map((project) => ({ project, inspection: inspect(project.root) }));
  const matches = candidates.filter(({ inspection }) => {
    const a = expected.identity;
    const b = inspection.identity;
    return a && b && a.dev === b.dev && a.ino === b.ino;
  });
  const unreadable = candidates.filter(({ inspection }) => !inspection.identity).length;
  if (expected.identity && matches.length === 1 && unreadable === 0) return matches[0]!.project;

  const details = candidates
    .slice(0, 8)
    .map(
      ({ project, inspection }) => `id=${project.id} root=${describe(project.root, inspection)}`,
    );
  if (candidates.length > 8) details.push(`${candidates.length - 8} candidates omitted`);
  throw new Error(
    `registered project lookup failed: matches=${matches.length}, unreadable=${unreadable}, ` +
      `candidates=${candidates.length}; expected=${describe(root, expected)}; ${details.join('; ')}`,
  );
}
