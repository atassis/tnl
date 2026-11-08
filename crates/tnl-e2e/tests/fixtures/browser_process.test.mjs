import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import test from 'node:test';
import {stopChild} from './browser_process.mjs';

async function bounded(promise, ms = 1500) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error('cleanup deadline exceeded')), ms);
    })]);
  } finally {
    clearTimeout(timer);
  }
}

test('cleanup of an already signal-terminated child must not await another exit', async () => {
  const child = spawn(process.execPath, ['-e', 'process.kill(process.pid, "SIGTERM")']);
  await once(child, 'exit');
  assert.equal(child.exitCode, null);
  assert.equal(child.signalCode, 'SIGTERM');
  await bounded(stopChild(child, 'SIGTERM', 20));
});

test('cleanup joins a live child terminated by signal', async () => {
  const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)']);
  try {
    await bounded(stopChild(child, 'SIGTERM', 20));
    assert.equal(child.signalCode, 'SIGTERM');
  } finally {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
  }
});

test('cleanup escalates when a ready child ignores SIGTERM', async () => {
  const child = spawn(process.execPath, ['-e', 'process.on("SIGTERM", () => {}); process.stdout.write("ready"); setInterval(() => {}, 1000)']);
  try {
    await bounded(once(child.stdout, 'data'));
    await bounded(stopChild(child, 'SIGTERM', 20));
    assert.equal(child.signalCode, 'SIGKILL');
  } finally {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
  }
});
