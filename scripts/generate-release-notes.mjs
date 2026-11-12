import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { generateNotes } from "@semantic-release/release-notes-generator";

function git(args) {
  return execFileSync("git", args, { encoding: "utf8" }).trim();
}

function fail(message) {
  console.error(`Could not generate release notes: ${message}`);
  process.exit(1);
}

const tag = process.env.GITHUB_REF_NAME;
const repositoryUrl = process.env.REPOSITORY_URL;
const outputPath = resolve(process.env.RELEASE_NOTES_PATH || "release-notes.md");
if (!/^v\d+\.\d+\.\d+$/.test(tag || "")) fail(`invalid release tag "${tag || ""}".`);
if (!repositoryUrl) fail("REPOSITORY_URL is required.");

const tags = git(["tag", "--list", "v[0-9]*", "--sort=-version:refname"])
  .split("\n")
  .filter((item) => /^v\d+\.\d+\.\d+$/.test(item));
const currentTagIndex = tags.indexOf(tag);
if (currentTagIndex < 0) fail(`tag ${tag} is missing from the checked-out repository.`);
const previousTag = tags.slice(currentTagIndex + 1).find((item) => item !== tag);
const revisionRange = previousTag ? `${previousTag}..${tag}` : tag;
const commitRecords = git(["log", "--format=%H%x00%B%x00%x1e", revisionRange])
  .split("\x1e")
  .map((record) => record.trim())
  .filter(Boolean);
const commits = commitRecords.map((record) => {
  const separator = record.indexOf("\0");
  return {
    hash: record.slice(0, separator).trim(),
    message: record.slice(separator + 1).replace(/\0$/, "").trim(),
  };
});

const lastRelease = previousTag
  ? {
      gitTag: previousTag,
      version: previousTag.slice(1),
      gitHead: git(["rev-parse", `${previousTag}^{commit}`]),
    }
  : { gitTag: null, version: "0.0.0", gitHead: null };
const nextRelease = {
  gitTag: tag,
  version: tag.slice(1),
  gitHead: git(["rev-parse", `${tag}^{commit}`]),
};
const notes = await generateNotes(
  { preset: "conventionalcommits" },
  {
    commits,
    lastRelease,
    nextRelease,
    options: { repositoryUrl },
    cwd: process.cwd(),
  },
);

mkdirSync(dirname(outputPath), { recursive: true });
writeFileSync(outputPath, notes.trim() ? `${notes.trim()}\n` : "No additional release notes.\n");
console.log(`Generated release notes for ${tag} at ${outputPath}`);
