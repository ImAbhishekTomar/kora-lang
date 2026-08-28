#!/usr/bin/env node
"use strict";

const path = require("path");
const { spawnSync } = require("child_process");

const bin = path.join(__dirname, process.platform === "win32" ? "kora-bin.exe" : "kora-bin");
const result = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });

if (result.error) {
  console.error(`kora-cli: could not run '${bin}': ${result.error.message}`);
  console.error("kora-cli: try reinstalling this package.");
  process.exit(1);
}
process.exit(result.status === null ? 1 : result.status);
