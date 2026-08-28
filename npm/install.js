#!/usr/bin/env node
// Downloads the kora release binary matching this package's version and the
// current OS/arch, so `npm install kora-cli` gives a working `kora` on PATH
// without shipping every platform's binary in the npm tarball.
"use strict";

const fs = require("fs");
const os = require("os");
const path = require("path");
const https = require("https");
const { pipeline } = require("stream/promises");
const tar = require("tar");

const REPO = "ImAbhishekTomar/kora-lang";
const { version } = require("./package.json");

function target() {
  const platform = { linux: "unknown-linux-gnu", darwin: "apple-darwin", win32: "pc-windows-msvc" }[os.platform()];
  const arch = { x64: "x86_64", arm64: "aarch64" }[os.arch()];
  if (!platform || !arch) {
    throw new Error(`unsupported platform: ${os.platform()}/${os.arch()}`);
  }
  return `${arch}-${platform}`;
}

function get(url, redirects = 5) {
  return new Promise((resolve, reject) => {
    https
      .get(url, { headers: { "User-Agent": "kora-cli-npm-installer" } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          if (redirects === 0) return reject(new Error("too many redirects"));
          res.resume();
          return resolve(get(res.headers.location, redirects - 1));
        }
        if (res.statusCode !== 200) {
          return reject(new Error(`GET ${url} -> ${res.statusCode}`));
        }
        resolve(res);
      })
      .on("error", reject);
  });
}

async function main() {
  const t = target();
  const windows = os.platform() === "win32";
  const binDir = path.join(__dirname, "bin");
  fs.mkdirSync(binDir, { recursive: true });

  if (windows) {
    const url = `https://github.com/${REPO}/releases/download/v${version}/kora-${version}-${t}.zip`;
    throw new Error(
      `automatic Windows install isn't wired up yet: download and extract\n${url}\nand put kora.exe on your PATH.`
    );
  }

  const archive = `kora-${version}-${t}.tar.gz`;
  const url = `https://github.com/${REPO}/releases/download/v${version}/${archive}`;
  console.log(`kora-cli: downloading ${archive}`);

  const extractDir = fs.mkdtempSync(path.join(os.tmpdir(), "kora-npm-"));
  const res = await get(url);
  await pipeline(res, tar.extract({ cwd: extractDir }));

  const found = fs
    .readdirSync(extractDir, { recursive: true })
    .map((p) => path.join(extractDir, p))
    .find((p) => path.basename(p) === "kora" && fs.statSync(p).isFile());
  if (!found) throw new Error(`'kora' binary not found in ${archive}`);

  const dest = path.join(binDir, "kora-bin");
  fs.copyFileSync(found, dest);
  fs.chmodSync(dest, 0o755);
  fs.rmSync(extractDir, { recursive: true, force: true });

  console.log("kora-cli: installed");
}

main().catch((err) => {
  console.error(`kora-cli: install failed: ${err.message}`);
  process.exit(1);
});
