var untouched = 1; // UNCHANGED
const fs = require('fs'); // OLD:prefer-node-protocol:modern
var count = 1; // OLD:no-var:modern
let fixed = 2; // DECOY
const kept = 3; // DECOY
function ok(u) {
  try { // OLD:prefer-url-can-parse:needs-newer
    new URL(u);
    return true;
  } catch {
    return false;
  }
}
const ok2 = (u) => URL.canParse(u); // DECOY
let resolve;
const p = new Promise((r) => { resolve = r; }); // OLD:prefer-promise-with-resolvers:needs-newer
const axios = require('axios'); // OLD:http-client-package:modern
const letters = /[a-z]/v; // OLD:es-x/no-regexp-v-flag:baseline
const lastBig = [1, 2].findLast((x) => x > 1); // DECOY
const grouped = [{ k: 1 }].reduce((acc, x) => { (acc[x.k] ??= []).push(x); return acc; }, {}); // OLD:prefer-group-by:needs-newer
[1].forEach((n) => n); // OLD:no-for-each:modern
const fromAsync = (async () => { const out = []; for await (const v of gen()) out.push(v); return out; })(); // OLD:prefer-array-from-async:needs-newer
const escaped = 'a.b'.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'); // OLD:prefer-regexp-escape:needs-newer
module.exports = { untouched, fs, count, fixed, kept, ok, ok2, p, resolve, axios, letters, lastBig, grouped, fromAsync, escaped };
