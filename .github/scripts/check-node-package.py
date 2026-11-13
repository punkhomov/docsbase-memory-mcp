#!/usr/bin/env python3
"""Check the actual local npm tarball offline, including its public JS and .d.ts."""
import json
from pathlib import Path, PurePosixPath
import subprocess
import sys
import tarfile
import tempfile


def check(archive_dir: Path) -> None:
    archives = list(archive_dir.glob("*.tgz"))
    if len(archives) != 1:
        raise RuntimeError(f"expected exactly one client archive, got {len(archives)}")
    repo = Path(__file__).resolve().parents[2]
    client = repo / "packages/client"
    with tempfile.TemporaryDirectory(prefix="docsbase-package-") as temporary:
        root = Path(temporary)
        installed = root / "node_modules/@docsbase/client"
        with tarfile.open(archives[0], "r:gz") as archive:
            for member in archive.getmembers():
                path = PurePosixPath(member.name)
                if path.is_absolute() or ".." in path.parts or not path.parts or path.parts[0] != "package":
                    raise RuntimeError("unsafe npm archive path")
                if member.isdir():
                    continue
                if not member.isfile():
                    raise RuntimeError("unexpected npm archive link/type")
                relative = PurePosixPath(*path.parts[1:])
                if not (relative.parts[0] == "dist" or str(relative) in {"package.json", "README.md", "LICENSE"}):
                    raise RuntimeError(f"unexpected npm artifact: {relative}")
                source = archive.extractfile(member)
                if source is None:
                    raise RuntimeError("unreadable npm artifact")
                target = installed.joinpath(*relative.parts)
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(source.read())
        package = json.loads((installed / "package.json").read_text())
        assert package["name"] == "@docsbase/client"
        assert not package.get("dependencies"), "runtime dependencies leaked into client"
        exports = package["exports"]
        assert set(exports) == {"."}, "raw transport subpaths must not be exported"
        for value in exports["."].values():
            assert value.startswith("./dist/") and (installed / value).is_file(), value
        assert (installed / "LICENSE").is_file()
        subprocess.run(["node", "--input-type=module", "-e", """
import assert from 'node:assert/strict';
import * as sdk from '@docsbase/client';
assert.deepEqual(Object.keys(sdk).sort(), ['DocsbaseClient','DocsbaseError']);
const endpoint = process.platform === 'win32' ? String.raw`\\\\.\\pipe\\docsbase-package-test` : '/tmp/docsbase-package-test.sock';
const client = new sdk.DocsbaseClient({endpoint,projectId:1,expectedBuildId:'test',expectedSchemaVersion:2,
settings:{searchConnections:1,controlConnections:1,queueLimit:0,requestTimeoutMs:1000}});
client.close(); await assert.rejects(client.search('x'), {kind:'closed'});
"""], cwd=root, check=True, timeout=30)
        consumer = root / "consumer.mts"
        consumer.write_text("""
import { DocsbaseClient, DocsbaseError, type ClientOptions, type SearchHit } from '@docsbase/client';
declare const settings: ClientOptions;
const client = new DocsbaseClient(settings);
const result = await client.search('query', {query_mode:'literal', dedupe:'path', n_per_doc:1});
const hits = Array.isArray(result) ? result : result.hits;
const hit: SearchHit | undefined = hits[0];
if (hit) { const exact: string = hit.chunk_id; void exact; }
// @ts-expect-error u64 identifiers must never become lossy JS numbers
const lossy: number = hits[0]!.chunk_id;
// @ts-expect-error arbitrary project selection is not a search option
await client.search('x', {projectId:2});
const status = await client.status(); const generation: number = status.project.index_generation;
void generation; void lossy; void new DocsbaseError('closed', 'closed'); client.close();
""")
        subprocess.run(["node", str((client / "node_modules/typescript/bin/tsc").resolve()),
                        "--noEmit", "--strict", "--module", "NodeNext", "--moduleResolution", "NodeNext",
                        "--target", "ES2024", "--types", "node", "--typeRoots",
                        str(client / "node_modules/@types"), str(consumer)],
                       cwd=root, check=True, timeout=60)
    print("Node client npm artifact: exports, offline JS, exact-ID declarations and no runtime deps OK")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: check-node-package.py <directory-containing-one-tgz>")
    check(Path(sys.argv[1]).resolve())
