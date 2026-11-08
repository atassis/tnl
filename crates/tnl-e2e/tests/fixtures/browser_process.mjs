export function hasExited(child) {
  return !child.pid || child.exitCode !== null || child.signalCode !== null;
}

export async function stopChild(child, signal = 'SIGTERM', graceMs = 2500, killMs = 1000) {
  if (hasExited(child)) return;
  let onExit;
  const exited = new Promise(resolve => {
    onExit = resolve;
    child.once('exit', onExit);
  });
  async function wait(ms) {
    let timer;
    try {
      return await Promise.race([
        exited.then(() => true),
        new Promise(resolve => { timer = setTimeout(() => resolve(false), ms); }),
      ]);
    } finally {
      clearTimeout(timer);
    }
  }
  try {
    child.kill(signal);
    if (hasExited(child) || await wait(graceMs)) return;
    child.kill('SIGKILL');
    if (!hasExited(child) && !await wait(killMs)) {
      throw new Error('owned fixture child did not exit after SIGKILL');
    }
  } finally {
    child.removeListener('exit', onExit);
  }
}
