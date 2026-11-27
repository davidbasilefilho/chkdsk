import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { basename, join } from "node:path";
import { assetPaths } from "./assets";
import { root } from "./windows";

export const repository = "davidbasilefilho/chkdsk";
export const workflow = ".github/workflows/validation.yml";
const requiredJobs = ["validation", "distribution", "smoke (windows-2022, x64,x64-baseline)", "smoke (windows-11-arm, arm64)"];
export interface Api {
  get<T>(path: string): T;
  pages<T>(path: string, key?: string): T[];
}
interface Ref { ref: string; sha: string; repo: { full_name: string } | null }
interface Pull { number: number; state: string; draft: boolean; head: Ref; base: Ref }
interface Run {
  id: number; path: string; event: string; head_branch: string; head_sha: string; status: string; conclusion: string;
  run_attempt: number; run_started_at: string; head_repository: { full_name: string };
  pull_requests: {number: number; head: {sha: string}; base: {sha: string; ref: string}}[];
}
interface Artifact {
  id: number; name: string; expired: boolean; digest: string; created_at: string;
  workflow_run: {id: number; head_sha: string; head_repository_id: number; repository_id: number};
}
interface Asset { id: number; name: string; state: string; digest: string }
export interface Selection {
  channel: string; sha: string; names: string[]; artifact?: { id: number; digest: string; run: number };
  assets?: Asset[]; release?: number;
}
export class Unavailable extends Error {}
function requireChannel(condition: unknown, detail: string): asserts condition {
  if (!condition) throw new Unavailable(detail);
}
const fullSha = /^[a-f0-9]{40}$/;
const sha256 = /^sha256:[a-f0-9]{64}$/;
const localWorkflow = readFileSync(join(root, workflow), "utf8").replaceAll("\r\n", "\n");
const policy = createHash("sha1").update("blob " + Buffer.byteLength(localWorkflow) + "\0" + localWorkflow).digest("hex");
function content(api: Api, name: string, ref: string) {
  return api.get<{sha: string; content: string}>("contents/" + name + "?ref=" + ref);
}
function textAt(api: Api, name: string, ref: string) {
  return Buffer.from(content(api, name, ref).content, "base64").toString();
}

function preview(api: Api, channel: string, sha: string, branch: string, pull?: Pull): Selection {
  requireChannel(fullSha.test(sha), "Invalid source SHA");
  const event = pull ? "pull_request" : "push";
  const runs = api.pages<Run>("actions/workflows/validation.yml/runs?event=" + event + "&head_sha=" + sha + "&per_page=100", "workflow_runs");
  const run = runs.filter(r => r.event === event && r.path === workflow && r.head_sha === sha && r.head_branch === branch
    && r.head_repository?.full_name === repository && (!pull || r.pull_requests.some(p =>
      p.number === pull.number && p.head.sha === sha && p.base.ref === pull.base.ref && p.base.sha === pull.base.sha)))
    .sort((a, b) => b.id - a.id)[0];
  requireChannel(run?.status === "completed" && run.conclusion === "success", "No successful current validation for " + channel);
  requireChannel(content(api, workflow, sha).sha === policy, "Unrecognized producer workflow");
  if (pull) {
    // A PR runs the merged workflow. If the base changed it independently, require
    // that the base already contains the approved version or reject the run.
    const base = content(api, workflow, pull.base.sha).sha;
    if (base !== policy) {
      const comparison = api.get<{merge_base_commit: {sha: string}}>("compare/" + pull.base.sha + "..." + sha);
      requireChannel(content(api, workflow, comparison.merge_base_commit.sha).sha === base, "Ambiguous merged workflow");
    }
  }
  const jobs = api.pages<{name: string; conclusion: string}>("actions/runs/" + run.id + "/attempts/" + run.run_attempt + "/jobs?per_page=100", "jobs");
  requireChannel(requiredJobs.every(name => jobs.filter(j => j.name === name && j.conclusion === "success").length === 1),
    "Incomplete distribution or native smoke checks");
  const matches = api.pages<Artifact>("actions/runs/" + run.id + "/artifacts?per_page=100", "artifacts")
    .filter(a => a.name === "windows-dev-" + sha);
  requireChannel(matches.length === 1, "Missing or ambiguous distribution artifact");
  const a = matches[0];
  requireChannel(!a.expired && sha256.test(a.digest) && a.workflow_run.id === run.id && a.workflow_run.head_sha === sha
    && a.workflow_run.repository_id === a.workflow_run.head_repository_id
    && Date.parse(a.created_at) >= Date.parse(run.run_started_at), "Unverified or expired artifact");
  const pkg = (Bun.TOML.parse(textAt(api, "Cargo.toml", sha)) as {package: {name: string; version: string}}).package;
  requireChannel(pkg.name === "chkdsk" && /^\d+\.\d+\.\d+(?:-[\w.-]+)?$/.test(pkg.version), "Unexpected package identity");
  return { channel, sha, names: assetPaths("", "chkdsk", pkg.version, "dev", sha).map(f => basename(f)),
    artifact: { id: a.id, digest: a.digest, run: run.id } };
}

function eligible(p: Pull) {
  return p.state === "open" && !p.draft && ["main", "dev"].includes(p.base.ref)
    && p.base.repo?.full_name === repository && p.head.repo?.full_name === repository;
}

export function select(api: Api, channel: string): Selection {
  if (channel === "latest") {
    const main = api.get<{commit: {sha: string}}>("branches/main").commit.sha;
    const { tag, sha } = JSON.parse(textAt(api, "channels/latest.json", main));
    requireChannel(typeof tag === "string" && /^v\d+\.\d+\.\d+$/.test(tag) && fullSha.test(sha), "Invalid stable pointer");
    const release = api.get<{id: number; draft: boolean; prerelease: boolean; tag_name: string; target_commitish: string; assets: Asset[]}>("releases/tags/" + tag);
    requireChannel(!release.draft && !release.prerelease && release.tag_name === tag && release.target_commitish === sha
      && api.get<{sha: string}>("commits/refs/tags/" + tag).sha === sha
      && ["ahead", "identical"].includes(api.get<{status: string}>("compare/" + sha + "..." + main).status), "Stable source mismatch");
    const names = assetPaths("", "chkdsk", tag.slice(1), "stable", sha).map(f => basename(f));
    requireChannel(release.assets.length === names.length && names.every(n =>
      release.assets.filter(a => a.name === n && a.state === "uploaded" && sha256.test(a.digest)).length === 1),
      "Approved stable release has no verified three-executable distribution");
    return { channel, sha, names, release: release.id, assets: release.assets.map(({id,name,state,digest}) => ({id,name,state,digest})).sort((a,b) => a.id-b.id) };
  }
  if (channel === "dev") return preview(api, channel, api.get<{commit: {sha: string}}>("branches/dev").commit.sha, "dev");
  if (/^pr-[1-9][0-9]*$/.test(channel)) {
    const p = api.get<Pull>("pulls/" + channel.slice(3));
    requireChannel(eligible(p), "PR is closed, draft or outside the eligible repository/branches");
    return preview(api, channel, p.head.sha, p.head.ref, p);
  }
  if (channel !== "pr") throw Error("Channel must be latest, dev, pr or pr-N");
  const candidates = api.pages<Pull>("pulls?state=open&per_page=100").filter(eligible).map(p => {
    const commit = api.get<{commit: {committer: {date: string}}}>("commits/" + p.head.sha);
    const time = Date.parse(commit.commit.committer.date);
    requireChannel(Number.isFinite(time), "Invalid commit timestamp");
    return { p, time };
  }).sort((a, b) => b.time - a.time || b.p.number - a.p.number);
  for (const {p} of candidates) {
    try { return preview(api, "pr-" + p.number, p.head.sha, p.head.ref, p); }
    catch (error) { if (!(error instanceof Unavailable)) throw error; }
  }
  throw new Unavailable("No eligible verified PR");
}
