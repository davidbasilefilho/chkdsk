import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { api, checkCurrent, download } from "./channel-install";
import { previewTag, publicationChannel, repository, select, Unavailable, type Run, type Selection } from "./channels";
import { publishRelease } from "./github";

const id = Number(process.env.VALIDATION_RUN);
if (!Number.isSafeInteger(id) || id < 1) throw Error("Invalid validation run ID");
const route = "actions/runs/" + id;
const trigger = api.get<Run>(route);
const channel = publicationChannel(trigger, repository);
if (!channel || trigger.id !== id) {
  console.log("Validation run is outside the preview release policy");
  process.exit(0);
}
const resolve = () => select(api, channel, true);
let selection: Selection;
try { selection = resolve(); }
catch (error) {
  if (!(error instanceof Unavailable)) throw error;
  console.log("Preview not eligible: " + error.message);
  process.exit(0);
}
if (selection.sha !== trigger.head_sha || selection.artifact?.run !== id
  || String(trigger.run_attempt) !== process.env.VALIDATION_ATTEMPT) {
  console.log("Superseded validation run");
  process.exit(0);
}
const version = selection.names[0].match(/^disksonar-(.+)-dev-[a-f0-9]{7}-windows-arm64\.exe$/)?.[1];
if (!version) throw Error("Unexpected preview asset name");
const tag = previewTag(channel, version, selection.sha);
const gitTagRoute = "git/matching-refs/tags/" + tag;
function checkTag(expectFound: boolean) {
  const refs = api.get<{ref: string; object: {type: string; sha: string}}[]>(gitTagRoute)
    .filter(ref => ref.ref === "refs/tags/" + tag);
  if ((expectFound && refs.length !== 1) || refs.some(ref => ref.object.type !== "commit" || ref.object.sha !== selection.sha))
    throw Error("Preview tag points at an unexpected commit");
}
const temp = mkdtempSync(join(tmpdir(), "disksonar-preview-"));
try {
  download(selection, temp);
  const check = () => {
    checkCurrent(selection, resolve());
    const current = api.get<Run>(route);
    if (current.run_attempt !== trigger.run_attempt || publicationChannel(current, repository) !== channel)
      throw Error("Validation changed before publication");
    checkTag(false);
  };
  publishRelease(repository, tag, selection.sha, true,
    `Preview for ${channel} at ${selection.sha}. Verified Windows binaries and SHA-256 manifest from [validation run ${id}](https://github.com/${repository}/actions/runs/${id}).`,
    selection.names.map(name => join(temp, name)), undefined, undefined, check);
  checkTag(true);
} finally { rmSync(temp, { recursive: true, force: true }); }
