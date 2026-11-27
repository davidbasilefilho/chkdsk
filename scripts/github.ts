import { mkdtempSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { digest } from "./assets";
import { CommandError, run } from "./windows";

export interface Asset { name: string; size: number; state: string; digest?: string }
export interface Release {
  id: number; tag_name: string; name: string; target_commitish: string;
  draft: boolean; prerelease: boolean; assets: Asset[];
}

export function publishRelease(repo: string, tag: string, sha: string, prerelease: boolean, notes: string,
  files: string[], command = run, sleep = Bun.sleepSync) {
  const endpoint = "repos/" + repo + "/releases";
  function api<T>(args: string[]): T {
    return JSON.parse(command(["gh", "api", ...args], true));
  }
  function lookup() {
    for (let attempt = 0; attempt < 4; attempt++) {
      const pages = api<Release[][]>(["--paginate", "--slurp", endpoint]);
      const matches = pages.flat().filter(item => item.tag_name === tag);
      if (matches.length > 1) throw Error("multiple GitHub releases found for tag " + tag);
      if (matches.length) return matches[0].id;
      if (attempt < 3) sleep(250 * (attempt + 1));
    }
  }
  const identity = { tag_name: tag, name: tag, target_commitish: sha, prerelease };
  const fields = Object.entries(identity).flatMap(([key, value]) => [typeof value === "boolean" ? "-F" : "-f", key + "=" + value]);
  let id = lookup();
  let created: Release | undefined;
  if (id === undefined) {
    try {
      created = api<Release>(["--method", "POST", endpoint, ...fields, "-f", "body=" + notes, "-F", "draft=true"]);
      id = created.id;
    } catch (error) {
      if (!(error instanceof CommandError)) throw error;
      id = lookup();
      if (id === undefined) throw error;
    }
  }
  if (!Number.isSafeInteger(id) || id <= 0) throw Error("invalid release ID");
  const url = endpoint + "/" + id;
  function fetchRelease(): Release {
    for (let attempt = 0; ; attempt++) {
      try { return api<Release>([url]); }
      catch (error) {
        if (!(error instanceof CommandError) || !error.stderr.includes("404") || attempt === 3) throw error;
        sleep(250 * (attempt + 1));
      }
    }
  }
  function verifyIdentity(release: Release, draft: boolean) {
    if (release.draft !== draft || release.tag_name !== tag || release.name !== tag
      || release.target_commitish !== sha || release.prerelease !== prerelease) throw Error("release identity mismatch");
  }
  function indexAssets(release: Release) {
    const assets = new Map(release.assets.map(asset => [asset.name, asset]));
    if (assets.size !== release.assets.length) throw Error("release contains duplicate asset names");
    return assets;
  }
  function matches(asset: Asset | undefined, file: string) {
    if (!asset || asset.state !== "uploaded" || asset.size !== statSync(file).size) return false;
    const expected = digest(file);
    if (typeof asset.digest === "string" && /^sha256:[a-f0-9]{64}$/i.test(asset.digest))
      return asset.digest.toLowerCase() === "sha256:" + expected;
    const dir = mkdtempSync(join(tmpdir(), "chkdsk-asset-"));
    try {
      command(["gh", "release", "download", tag, "--repo", repo, "--pattern", basename(file), "--dir", dir]);
      return digest(join(dir, basename(file))) === expected;
    } finally { rmSync(dir, { recursive: true, force: true }); }
  }
  function verifyRemote(release: Release) {
    const remote = indexAssets(release);
    if (remote.size !== files.length || files.some(file => !matches(remote.get(basename(file)), file)))
      throw Error("release asset names, state, size or content do not match");
  }
  let release = created ?? fetchRelease();
  if (release.draft === false) {
    verifyIdentity(release, false);
    verifyRemote(release);
    console.log("verified already-published release " + tag);
    return;
  }
  // Repair only the prerelease flag on an existing draft with the expected identity.
  verifyIdentity({ ...release, prerelease }, true);
  if (release.prerelease !== prerelease) api(["--method", "PATCH", url, ...fields, "-F", "draft=true"]);
  release = fetchRelease();
  verifyIdentity(release, true);
  const remote = indexAssets(release);
  const names = files.map(file => basename(file));
  for (const name of remote.keys()) {
    if (!names.includes(name)) command(["gh", "release", "delete-asset", tag, "--yes", "--repo", repo, "--", name]);
  }
  const upload = files.filter(file => !matches(remote.get(basename(file)), file));
  if (upload.length) command(["gh", "release", "upload", tag, ...upload, "--clobber", "--repo", repo]);
  release = fetchRelease();
  verifyIdentity(release, true);
  verifyRemote(release);
  api(["--method", "PATCH", url, ...fields, "-F", "draft=false"]);
  verifyIdentity(fetchRelease(), false);
  console.log("published verified release " + tag);
}
