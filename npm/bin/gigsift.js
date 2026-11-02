#!/usr/bin/env node
"use strict";

// This package doesn't include the gigsift program. It runs an installed copy, or
// says how to get one. npm puts this script on PATH as `gigsift` too, so the search
// has to skip it, or it would run itself forever.

const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const BINARY = process.platform === "win32" ? "gigsift.exe" : "gigsift";

function findBinary() {
  const self = fs.realpathSync(__filename);
  const folders = (process.env.PATH ?? "").split(path.delimiter);
  folders.push(path.join(os.homedir(), ".cargo", "bin"));

  for (const folder of folders.filter(Boolean)) {
    const candidate = path.join(folder, BINARY);
    try {
      const real = fs.realpathSync(candidate);
      if (real !== self && fs.statSync(real).isFile()) return real;
    } catch {
      // Not in this folder.
    }
  }
  return undefined;
}

const binary = findBinary();
if (!binary) {
  console.error(`gigsift isn't installed. This npm package doesn't include the program itself.

Install it with Cargo:

  cargo install gigsift

Then make sure Cargo's bin folder is on your PATH.`);
  process.exit(1);
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
if (result.error) {
  console.error(`Couldn't run ${binary}: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
