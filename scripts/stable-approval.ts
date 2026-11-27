import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";

const repo = "davidbasilefilho/chkdsk";
export function requireStableApproval() {
  const pointer = JSON.parse(readFileSync("channels/latest.json", "utf8"));
  const { STABLE_SHA: sha, STABLE_TAG: tag, GITHUB_SHA: workflowSha } = process.env;
  if (process.env.GITHUB_EVENT_NAME !== "workflow_dispatch" || process.env.GITHUB_REPOSITORY !== repo || process.env.GITHUB_REF !== "refs/heads/main"
      || !sha || !/^[0-9a-f]{40}$/.test(sha) || !tag || !/^v\d+\.\d+\.\d+$/.test(tag)
      || tag !== pointer.tag || sha !== pointer.sha) throw Error("Stable promotion does not match the reviewed main pointer");
  function api(path: string) {
    return JSON.parse(execFileSync("gh", ["api", "repos/" + repo + "/" + path], { encoding: "utf8" }));
  }
  if (api("branches/main").commit.sha !== workflowSha || api("git/ref/tags/" + tag).ref !== "refs/tags/" + tag
      || api("commits/refs/tags/" + tag).sha !== sha
      || !["ahead", "identical"].includes(api("compare/" + sha + "..." + workflowSha).status))
    throw Error("Stable ref changed or tag does not belong to main");
}
if (import.meta.main) requireStableApproval();
