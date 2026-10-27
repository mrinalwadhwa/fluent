// Diagnose a failed Writer launch inside its unchanged worker profile. Print
// only operation results and paths, never the inherited environment or npmrc.
const fs = require('node:fs');
const path = require('node:path');
const { spawn } = require('node:child_process');
const npm = process.argv[1];
const makeSpawnArgs = require(path.join(npm, 'node_modules/@npmcli/run-script/lib/make-spawn-args.js'));
const promiseSpawn = require(path.join(npm, 'node_modules/@npmcli/promise-spawn'));
const [, , options] = makeSpawnArgs({
  cmd: 'exit 0', args: [], env: {}, event: 'test', path: process.cwd(), stdio: 'inherit',
});
options.timeout = 5_000;
options.killSignal = 'SIGKILL';

function report(name, result) {
  console.log(JSON.stringify({ case: name, ...result }));
}

async function probe(name, command, opts) {
  const result = await new Promise(resolve => {
    try {
      const child = spawn(command, ['-c', 'exit 0'], { ...opts, shell: false });
      child.on('error', error => resolve({ error: error.code, syscall: error.syscall }));
      child.on('exit', (status, signal) => resolve({ status, signal }));
      child.stdout?.resume();
      child.stderr?.resume();
    } catch (error) {
      resolve({ error: error.code, syscall: error.syscall });
    }
  });
  report(name, result);
}

(async () => {
  report('runtime', { node: process.version, uv: process.versions.uv, cwd: process.cwd() });
  for (const fd of [0, 1, 2]) {
    try {
      const stat = fs.fstatSync(fd);
      report('stdio', { fd, file: stat.isFile(), fifo: stat.isFIFO(), character: stat.isCharacterDevice() });
    } catch (error) {
      report('stdio', { fd, error: error.code });
    }
  }
  // Use the selected npm's own PATH builder and shell runner, including cwd
  // and inherited streams, so the first case matches its lifecycle launch.
  try {
    const result = await promiseSpawn('exit 0', [], options);
    report('npm-shell', { status: result.code });
  } catch (error) {
    report('npm-shell', { error: error.code, syscall: error.syscall });
  }
  await probe('absolute-shell', '/bin/sh', options);
  await probe('absolute-shell-without-chdir', '/bin/sh', { ...options, cwd: undefined });
  await probe('absolute-shell-piped', '/bin/sh', { ...options, stdio: 'pipe' });
  await probe('absolute-shell-ignored-stdio', '/bin/sh', { ...options, stdio: 'ignore' });
  await probe('system-path-shell', 'sh', {
    ...options, env: { ...options.env, PATH: '/usr/bin:/bin' },
  });
  await probe('missing-executable', path.join(process.cwd(), 'missing-spawn-probe'), options);
  await probe('missing-path-command', 'fluent-missing-path-probe', options);
  for (const directory of options.env.PATH.split(path.delimiter)) {
    const file = path.join(directory, 'sh');
    try {
      report('shell-path', { path: file, target: fs.realpathSync(file) });
    } catch (error) {
      report('shell-path', { path: file, error: error.code });
    }
  }
})().catch(error => {
  report('probe-failed', { error: error.code || error.name });
  process.exitCode = 1;
});
