import { test, expect } from "bun:test";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { assetPaths } from "./assets";
import { checkCurrent } from "./channel-install";
import { repository, select, workflow, type Api } from "./channels";

const sha = "1".repeat(40), older = "2".repeat(40), newer = "3".repeat(40);
const digest = "sha256:" + "4".repeat(64);
const contents = readFileSync(workflow, "utf8").replaceAll("\r\n", "\n");
const blob = createHash("sha1").update("blob " + Buffer.byteLength(contents) + "\0" + contents).digest("hex");
const names = assetPaths("", "disksonar", "1.0.0", "stable", sha);
const ref = (ref: string, sha: string) => ({ ref, sha, repo: {full_name: repository} });
function fixture() {
  const pulls = [older, newer].map((s,i) => ({number: i+1, state:"open", draft:false, base:ref("main",sha), head:ref("feature"+i,s)}));
  const runs = [sha, older, newer].map((s,i) => ({id: 100-i, status:"completed", conclusion:"success", path:workflow,
    event:i ? "pull_request" : "push", head_sha:s, head_branch:i ? pulls[i-1].head.ref : "dev", head_repository:{full_name:repository},
    pull_requests: i ? [pulls[i-1]] : [], run_attempt:1,run_started_at:"2026-01-01T00:00:00Z"}));
  const jobs = ["validation", "distribution", "smoke (windows-2022, x64,x64-baseline)", "smoke (windows-11-arm, arm64)"]
    .map(name => ({name,conclusion:"success"}));
  const release = {id:40,tag_name:"v1.0.0",target_commitish:sha,prerelease:false,draft:false,
    assets:names.map((name,id) => ({id,name,state:"uploaded",digest}))};
  const state = { branch:sha, workflowBlob:blob, expired:false, linked:true, date:"2026-01-02T00:00:00Z" };
  const api = {
    get(path: string) {
      if (path === "branches/main" || path === "branches/dev") return {commit:{sha:state.branch}};
      if (path.startsWith("contents/channels/latest.json")) return {content:Buffer.from(JSON.stringify({tag:"v1.0.0",sha})).toString("base64")};
      if (path.startsWith("contents/" + workflow)) return {sha:state.workflowBlob};
      if (path.startsWith("contents/Cargo.toml")) return {content:Buffer.from('[package]\nname="disksonar"\nversion="1.0.0"').toString("base64")};
      if (path.startsWith("pulls/")) return pulls[Number(path.split("/")[1])-1];
      if (path.startsWith("releases/tags/")) return release;
      if (path.startsWith("commits/refs/tags/")) return {sha};
      if (path.startsWith("compare/")) return {status:"ahead"};
      if (path.startsWith("commits/")) return {commit:{committer:{date:path.endsWith(newer) ? "2026-01-03" : "2026-01-01"}}};
      throw Error(path);
    },
    pages(path: string) {
      if (path.startsWith("pulls?")) return pulls;
      if (path.startsWith("actions/workflows/")) return runs;
      if (path.includes("/jobs?")) return jobs;
      const run = runs.find(r => path.startsWith("actions/runs/" + r.id + "/artifacts?"));
      if (!run) throw Error(path);
      return [{id:run.id+1000,name:"windows-dev-"+run.head_sha,digest,expired:state.expired,created_at:state.date,
        workflow_run:{id:state.linked ? run.id : 0,head_sha:run.head_sha,repository_id:1,head_repository_id:1}}];
    }
  } as Api;
  return {api,runs,jobs,pulls,state,release};
}

test("dev accepts only its verified push HEAD and latest only the approved three-executable release", () => {
  const f = fixture();
  expect(select(f.api,"dev").sha).toBe(sha);
  expect(select(f.api,"latest").release).toBe(40);
  f.release.assets.pop();
  expect(() => select(f.api,"latest")).toThrow("three-executable");
  f.state.branch = newer;
  expect(() => select(f.api,"dev")).toThrow("current validation");
});

test("PR number, state, repo, workflow, SHA and every native check must match", () => {
  const f = fixture();
  expect(select(f.api,"pr-1").sha).toBe(older);
  f.pulls[0].draft=true;
  expect(() => select(f.api,"pr-1")).toThrow();
  f.pulls[0].draft=false;
  f.pulls[0].head.repo.full_name="third-party/fork";
  expect(() => select(f.api,"pr-1")).toThrow();
  f.pulls[0].head.repo.full_name=repository;
  f.jobs.pop();
  expect(() => select(f.api,"pr-1")).toThrow("smoke");
});

test("aggregate orders eligible HEADs by commit time, ignoring run IDs/completion order", () => {
  const f = fixture();
  expect(select(f.api,"pr").channel).toBe("pr-2");
  f.runs[2].conclusion="failure";
  expect(select(f.api,"pr").channel).toBe("pr-1");
  f.pulls[0].state="closed";
  expect(() => select(f.api,"pr")).toThrow();
});

test("reject unknown workflow, expired/cross-run artifacts and artifacts from a previous attempt", () => {
  const f = fixture();
  f.state.workflowBlob="other";
  expect(() => select(f.api,"dev")).toThrow("producer");
  f.state.workflowBlob=blob;
  f.state.expired=true;
  expect(() => select(f.api,"dev")).toThrow("expired");
  f.state.expired=false; f.state.linked=false;
  expect(() => select(f.api,"dev")).toThrow();
  f.state.linked=true; f.state.date="2025-01-01";
  expect(() => select(f.api,"dev")).toThrow();
});

test("a new HEAD or artifact while downloading cannot replace the selected version", () => {
  const f = fixture(), before = select(f.api,"pr");
  checkCurrent(before,select(f.api,"pr"));
  f.pulls[1].state="closed";
  expect(() => checkCurrent(before,select(f.api,"pr"))).toThrow("changed");
});

test("only successful same-repository dev pushes and PR validation runs can trigger publication", async () => {
  const { publicationChannel } = await import("./channels");
  const f = fixture();
  const runs = f.api.pages<import("./channels").Run>("actions/workflows/");
  const dev = runs[0];
  const pr = runs[1];
  expect(publicationChannel(dev, repository)).toBe("dev");
  expect(publicationChannel(pr, repository)).toBe("pr-1");
  expect(publicationChannel({ ...dev, head_branch: "main" }, repository)).toBeUndefined();
  expect(publicationChannel({ ...pr, head_repository: { full_name: "other/fork" } }, repository)).toBeUndefined();
  expect(publicationChannel({ ...pr, conclusion: "cancelled" }, repository)).toBeUndefined();
  expect(publicationChannel({ ...pr, head_sha: dev.head_sha }, repository)).toBeUndefined();
  expect(publicationChannel({ ...pr, path: ".github/workflows/other.yml" }, repository)).toBeUndefined();
});
