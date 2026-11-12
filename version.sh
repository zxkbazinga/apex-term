#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "Usage: $0 <version> (example: $0 0.1.2)" >&2
  exit 2
fi

VERSION="$1"
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Version must use numeric MAJOR.MINOR.PATCH format, such as 0.1.2." >&2
  exit 2
fi

node - "$VERSION" <<'NODE'
const fs = require("node:fs");

const version = process.argv[2];
const packagePath = "package.json";
const packageLockPath = "package-lock.json";
const tauriPath = "src-tauri/tauri.conf.json";
const cargoPath = "src-tauri/Cargo.toml";
const cargoLockPath = "src-tauri/Cargo.lock";

function readJson(path) {
  return JSON.parse(fs.readFileSync(path, "utf8"));
}

const packageJson = readJson(packagePath);
const packageLock = readJson(packageLockPath);
const tauriConfig = readJson(tauriPath);
const cargoToml = fs.readFileSync(cargoPath, "utf8");
const cargoLock = fs.readFileSync(cargoLockPath, "utf8");

function findTomlSection(text, header) {
  const lines = text.split(/(?=^\[)/m);
  return lines.filter((section) => section.startsWith(`${header}\n`) || section.startsWith(`${header}\r\n`));
}

const workspacePackageSections = findTomlSection(cargoToml, "[workspace.package]");
const workspaceVersion = workspacePackageSections[0]?.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
const lockSections = cargoLock.split(/(?=^\[\[package\]\]$)/m);
const lockMatches = lockSections.filter((section) => /^name = "apex-term"$/m.test(section));
const cargoLockVersion = lockMatches[0]?.match(/^version = "([^"]+)"/m)?.[1];
const currentVersions = [
  packageJson.version,
  packageLock.version,
  packageLock.packages?.[""]?.version,
  workspaceVersion,
  cargoLockVersion,
];
const tauriVersionUsesPackageJson = tauriConfig.version === "../package.json";

if (
  currentVersions.some((item) => typeof item !== "string") ||
  lockMatches.length !== 1 ||
  (typeof tauriConfig.version !== "string") ||
  (!tauriVersionUsesPackageJson && tauriConfig.version !== packageJson.version)
) {
  throw new Error("Could not reliably locate every project version field; no files were changed.");
}
if (!currentVersions.every((item) => item === currentVersions[0])) {
  throw new Error(`Project version fields do not match: ${currentVersions.join(", ")}. No files were changed.`);
}
if (version === currentVersions[0]) {
  throw new Error(`Project is already at version ${version}.`);
}

packageJson.version = version;
packageLock.version = version;
packageLock.packages[""].version = version;
if (!tauriVersionUsesPackageJson) tauriConfig.version = version;

const nextCargoToml = cargoToml.replace(
  /^(\[workspace\.package\]\s*\n[\s\S]*?^version\s*=\s*")[^"]+(")/m,
  `$1${version}$2`,
);
const nextCargoLock = lockSections
  .map((section) => {
    if (!/^name = "apex-term"$/m.test(section)) return section;
    return section.replace(/^(version = ")[^"]+(")/m, `$1${version}$2`);
  })
  .join("");

if (nextCargoToml === cargoToml || nextCargoLock === cargoLock) {
  throw new Error("Could not update the Cargo version fields; no files were changed.");
}

fs.writeFileSync(packagePath, `${JSON.stringify(packageJson, null, 2)}\n`);
fs.writeFileSync(packageLockPath, `${JSON.stringify(packageLock, null, 2)}\n`);
if (!tauriVersionUsesPackageJson) {
  fs.writeFileSync(tauriPath, `${JSON.stringify(tauriConfig, null, 2)}\n`);
}
fs.writeFileSync(cargoPath, nextCargoToml);
fs.writeFileSync(cargoLockPath, nextCargoLock);

console.log(`Updated project version from ${currentVersions[0]} to ${version}.`);
NODE
