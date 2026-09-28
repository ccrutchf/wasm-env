// Node.js: the language, its standard library and its event loop, with
// deterministic output. Covers V8 (objects, closures, classes, generators,
// regular expressions, typed arrays, BigInt, JSON), promises and timers,
// the file system, zlib, crypto (OpenSSL), a child process, a worker
// thread and an HTTP round trip over the loopback interface.
'use strict';
const assert = require('assert');
const crypto = require('crypto');
const fs = require('fs');
const os = require('os');
const path = require('path');
const zlib = require('zlib');
const http = require('http');
const { execFileSync } = require('child_process');
const { Worker } = require('worker_threads');

function section(name, value) {
  console.log(`${name}: ${typeof value === 'string' ? value : JSON.stringify(value)}`);
}

// The language.
function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
section('fib', fib(25));
class Point {
  constructor(x, y) { this.x = x; this.y = y; }
  add(o) { return new Point(this.x + o.x, this.y + o.y); }
  get norm() { return Math.hypot(this.x, this.y); }
}
let p = new Point(0, 0);
for (let i = 0; i < 100000; i++) p = p.add(new Point(i % 3, i % 5));
section('points', [p.x, p.y, p.norm.toFixed(3)]);
function* primes() {
  const found = [];
  for (let n = 2; ; n++) if (found.every((q) => n % q)) { found.push(n); yield n; }
}
const first = [];
for (const q of primes()) { if (first.length === 20) break; first.push(q); }
section('primes', first);
const words = 'the quick brown fox jumps over the lazy dog'.split(' ');
const counts = words.reduce((m, w) => m.set(w, (m.get(w) || 0) + 1), new Map());
section('words', [...counts].sort().slice(0, 4));
section('regex', 'a1b22c333'.replace(/\d+/g, (d) => `<${d.length}>`));
section('bigint', (2n ** 100n).toString());
const floats = new Float64Array(1000).map((_, i) => Math.sin(i));
section('typed', floats.reduce((a, b) => a + b).toFixed(6));
section('json', JSON.parse(JSON.stringify({ a: [1, 2, { b: 'c' }], d: null })));
section('sort', Array.from({ length: 20 }, (_, i) => (i * 7919) % 101).sort((a, b) => a - b));
section('strings', ['é', 'ß', 'Σ'].map((s) => s.toUpperCase() + s.normalize('NFD').length));

// Buffers, crypto, zlib.
const text = Buffer.from('hello, wasm-env '.repeat(1000));
section('sha256', crypto.createHash('sha256').update(text).digest('hex'));
section('hmac', crypto.createHmac('sha1', 'key').update('data').digest('base64'));
const packed = zlib.gzipSync(text, { level: 9 });
section('gzip', [zlib.gunzipSync(packed).equals(text), zlib.inflateSync(zlib.deflateSync(text)).length]);
section('base64', Buffer.from('wasm-env').toString('base64'));

// The file system.
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'node-demo-'));
fs.writeFileSync(path.join(dir, 'a.txt'), 'first\nsecond\n');
fs.appendFileSync(path.join(dir, 'a.txt'), 'third\n');
fs.mkdirSync(path.join(dir, 'sub'));
fs.renameSync(path.join(dir, 'a.txt'), path.join(dir, 'sub', 'b.txt'));
section('fs', [fs.readdirSync(dir), fs.readFileSync(path.join(dir, 'sub', 'b.txt'), 'utf8').split('\n').length,
  fs.statSync(path.join(dir, 'sub', 'b.txt')).size]);
fs.rmSync(dir, { recursive: true });
section('fs cleaned', fs.existsSync(dir));

// A child process.
section('child', execFileSync('/bin/sh', ['-c', 'echo from sh; expr 6 \\* 7']).toString().trim().split('\n'));

// The event loop: promises, timers, a worker, HTTP.
(async () => {
  // (Each awaited in turn: which of a timer and an immediate runs first
  // depends on how long a turn of the loop takes.)
  const order = [];
  process.nextTick(() => order.push('tick'));
  await Promise.resolve().then(() => order.push('microtask'));
  await new Promise((r) => setImmediate(() => { order.push('immediate'); r(); }));
  await new Promise((r) => setTimeout(() => { order.push('timer'); r(); }, 20));
  section('event loop', order);

  const worker = new Worker('const { parentPort } = require("worker_threads");' +
    'parentPort.on("message", (n) => { let s = 0; for (let i = 1; i <= n; i++) s += i; parentPort.postMessage(s); });',
    { eval: true });
  const sum = await new Promise((resolve) => { worker.once('message', resolve); worker.postMessage(1000000); });
  await worker.terminate();
  section('worker', sum);

  const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', () => { res.setHeader('content-type', 'text/plain'); res.end(`${req.method} ${req.url} ${body.length}`); });
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const reply = await new Promise((resolve, reject) => {
    const req = http.request({ port: server.address().port, host: '127.0.0.1', method: 'POST', path: '/echo' }, (res) => {
      let data = '';
      res.on('data', (c) => { data += c; });
      res.on('end', () => resolve([res.statusCode, res.headers['content-type'], data]));
    });
    req.on('error', reject);
    req.end('x'.repeat(5000));
  });
  server.close();
  section('http', reply);
  assert.strictEqual(reply[0], 200);
  section('done', 'ok');
})();
