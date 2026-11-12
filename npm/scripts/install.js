#!/usr/bin/env node
'use strict';

const { spawnSync } = require('child_process');
const fs = require('fs');
const https = require('https');
const path = require('path');

const pkg = require('../package.json');

if (process.env.NEXIUM_SKIP_DOWNLOAD === '1') {
  process.exit(0);
}

const name = process.platform === 'win32' ? 'nexium.exe' : 'nexium';
const dir = path.join(
  __dirname,
  '..',
  'vendor',
  `${process.platform}-${process.arch}`,
);
const target = path.join(dir, name);

if (fs.existsSync(target)) {
  process.exit(0);
}

const asset = `nexium-v${pkg.version}-${process.platform}-${process.arch}.tar.gz`;
const url = `https://github.com/james3124/nexium/releases/download/v${pkg.version}/${asset}`;

function download(location, redirects = 0) {
  return new Promise((resolve, reject) => {
    https
      .get(location, { headers: { 'user-agent': 'nexium-npm-installer' } }, (response) => {
        const status = response.statusCode || 0;
        if ([301, 302, 303, 307, 308].includes(status) && response.headers.location) {
          response.resume();
          if (redirects >= 10) {
            reject(new Error('too many redirects'));
            return;
          }
          resolve(download(new URL(response.headers.location, location).toString(), redirects + 1));
          return;
        }
        if (status !== 200) {
          response.resume();
          reject(new Error(`HTTP ${status}`));
          return;
        }
        fs.mkdirSync(dir, { recursive: true });
        const archive = path.join(dir, asset);
        const file = fs.createWriteStream(archive);
        response.pipe(file);
        file.on('finish', () => file.close(() => resolve(archive)));
        file.on('error', reject);
      })
      .on('error', reject);
  });
}

(async () => {
  try {
    const archive = await download(url);
    const extracted = spawnSync('tar', ['-xzf', archive, '-C', dir], { stdio: 'inherit' });
    fs.rmSync(archive, { force: true });
    if (extracted.status !== 0) {
      throw new Error('tar extraction failed');
    }
    if (process.platform !== 'win32') {
      fs.chmodSync(target, 0o755);
    }
    console.log(`nexium: installed prebuilt binary for ${process.platform}/${process.arch}`);
  } catch (error) {
    console.warn(`nexium: could not download a prebuilt binary (${error.message})`);
    console.warn('nexium: set NEXIUM_BINARY_PATH or build from source:');
    console.warn('nexium: https://github.com/james3124/nexium#build-and-run');
  }
})();
