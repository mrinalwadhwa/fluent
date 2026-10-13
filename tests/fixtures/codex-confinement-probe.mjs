import fs from 'node:fs';
import net from 'node:net';

const [operation, path] = process.argv.slice(1);

function requirePermission(error) {
  if (error?.code !== 'EPERM' && error?.code !== 'EACCES') {
    throw error;
  }
  console.log(`${operation}: denied ${error.code}`);
}

function checkFile(action) {
  try {
    action();
  } catch (error) {
    requirePermission(error);
    return;
  }
  throw new Error(`${operation}: neighboring file was accessible`);
}

async function checkSocket() {
  await new Promise((resolve, reject) => {
    const socket = net.createConnection({ path });
    let settled = false;
    const finish = (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.destroy();
      if (error) reject(error);
      else resolve();
    };
    const timer = setTimeout(() => finish(new Error('connect: timed out')), 2000);
    socket.once('connect', () => finish(new Error('connect: neighboring socket was accessible')));
    socket.once('error', (error) => {
      try {
        requirePermission(error);
        finish();
      } catch (failure) {
        finish(failure);
      }
    });
  });
}

if (!path) throw new Error('missing neighboring resource path');
switch (operation) {
  case 'read':
    checkFile(() => fs.readFileSync(path));
    break;
  case 'write':
    checkFile(() => fs.writeFileSync(path, 'wrong', { flag: 'r+' }));
    break;
  case 'connect':
    await checkSocket();
    break;
  default:
    throw new Error(`unknown confinement operation: ${operation}`);
}
