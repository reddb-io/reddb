#!/usr/bin/env node
'use strict';

const path = require('node:path');
const { ensureInstalled } = require('./red-sdk');

const SKIP_TOKEN = '1';
const targetDir = path.join(__dirname, '.reddb', 'bin');
const shouldSkip = process.env.REDDB_SKIP_POSTINSTALL === SKIP_TOKEN;
const verify = process.env.REDDB_POSTINSTALL_NO_VERIFY !== SKIP_TOKEN;
const channel = process.env.REDDB_POSTINSTALL_CHANNEL;
const version = process.env.REDDB_POSTINSTALL_VERSION;
const options = {
  targetDir,
  verify
};

if (channel) {
  options.channel = channel;
}

if (version) {
  options.version = version;
}

if (process.env.GITHUB_TOKEN) {
  options.githubToken = process.env.GITHUB_TOKEN;
}

if (shouldSkip) {
  process.stdout.write('reddb: postinstall skipped by REDDB_SKIP_POSTINSTALL=1\n');
  process.exit(0);
}

ensureInstalled(options)
  .then((result) => {
    if (result.changed) {
      process.stdout.write(`reddb: installed binary at ${result.binaryPath}\n`);
    } else {
      process.stdout.write(`reddb: binary already installed at ${result.binaryPath}\n`);
    }
  })
  .catch((error) => {
    process.stderr.write(`reddb: postinstall skipped (${error.message})\n`);
    process.exit(0);
  });
