#!/usr/bin/env node
'use strict';

const { spawn } = require('child_process');
const fs = require('fs');
const path = require('path');

function binaryName() {
  return process.platform === 'win32' ? 'nexium.exe' : 'nexium';
}

function locateBinary() {
  if (process.env.NEXIUM_BINARY_PATH) {
    return process.env.NEXIUM_BINARY_PATH;
  }
  return path.join(
    path.resolve(__dirname, '..'),
    'vendor',
    `${process.platform}-${process.arch}`,
    binaryName(),
  );
}

const binary = locateBinary();
if (!fs.existsSync(binary)) {
  console.error(
    `nexium: no binary for ${process.platform}/${process.arch} at ${binary}\n` +
      'Retry the download with "npm rebuild nexiumx", set NEXIUM_BINARY_PATH,\n' +
      'or build from source: https://github.com/james3124/nexium#build-and-run',
  );
  process.exit(1);
}

const child = spawn(binary, process.argv.slice(2), { stdio: 'inherit' });

for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, () => {
    try {
      child.kill(signal);
    } catch {
      // child already exited
    }
  });
}

child.on('error', (error) => {
  console.error(`nexium: ${error.message}`);
  process.exit(1);
});

child.on('exit', (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
  } else {
    process.exit(code === null ? 1 : code);
  }
});
