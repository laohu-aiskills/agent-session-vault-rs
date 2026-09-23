const fs = require('fs'), crypto = require('crypto'), path = require('path');
const BS = String.fromCharCode(92);
const m = JSON.parse(fs.readFileSync(process.argv[3] + '/manifest.json', 'utf8'));
const base = process.argv[2];
const homeRel = (m.home || '').replace(/^[A-Za-z]:\\/, ''); // e.g. Users\22740
let ok = 0;
for (const f of m.files) {
  let rest = f.rel.slice(2); // 去掉 "c\"
  if (homeRel && rest.toLowerCase().startsWith(homeRel.toLowerCase())) {
    rest = rest.slice(homeRel.length).replace(/^[\\/]+/, '');
  }
  const full = path.join(base, ...rest.split(BS));
  const h = crypto.createHash('sha256').update(fs.readFileSync(full)).digest('hex');
  if (h === f.sha256) ok++; else console.log('MISMATCH', full);
}
console.log(`sha256 比对通过 ${ok}/${m.files.length}`);
process.exit(ok === m.files.length ? 0 : 1);
