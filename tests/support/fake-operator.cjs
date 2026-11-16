const fs = require('node:fs');
const path = require('node:path');
const dir = process.env.LAMBDA_MEMORY_DATA_DIR, project = process.env.LAMBDA_MEMORY_PROJECT;
const file = path.join(dir, 'sources.jsonl');
const read = () => fs.existsSync(file) ? fs.readFileSync(file, 'utf8').split('\n').filter(Boolean).map(JSON.parse) : [];
const curatable = row => row.kind !== 'action' && !row.origin.startsWith('omp://tool_result');
const rows = read();
if (process.argv[2] === '--inspect-sources') {
  console.log(JSON.stringify({ project, sources: rows.length, curatable: rows.filter(curatable).length, pending: 0, max_pending: 1000 }));
} else if (process.argv[2] === '--export-sources') {
  for (const row of rows) console.log(JSON.stringify(row));
} else if (process.argv[2] === '--import-sources') {
  const input = fs.readFileSync(0, 'utf8').split('\n').filter(Boolean).map(JSON.parse);
  if (input.length > Number(process.env.FAKE_OPERATOR_LINE_LIMIT ?? Infinity)) process.exit(2);
  if (process.env.FAKE_OPERATOR_IMPORT_LOG) fs.appendFileSync(process.env.FAKE_OPERATOR_IMPORT_LOG, `${input.length}\n`);
  let inserted = 0, duplicates = 0, remaining = 0, used = 0;
  const conflicts = [], rejected = [], headroom = Number(process.env.FAKE_OPERATOR_HEADROOM ?? Infinity);
  for (const row of input) {
    if (row.source === process.env.FAKE_OPERATOR_REJECT_SOURCE) { rejected.push(row.source); continue; }
    const existing = rows.find(item => item.source === row.source);
    if (existing) { if (existing.content === row.content) duplicates++; else conflicts.push(row.source); continue; }
    if (curatable(row) && used >= headroom) { remaining++; continue; }
    rows.push(row); inserted++; if (curatable(row)) used++;
  }
  fs.mkdirSync(dir, { recursive: true }); fs.writeFileSync(file, rows.map(row => JSON.stringify(row)).join('\n') + (rows.length ? '\n' : ''));
  if (process.env.FAKE_OPERATOR_FAIL_IMPORT === project || (process.env.FAKE_OPERATOR_FAIL_CHUNK && process.env.FAKE_OPERATOR_IMPORT_LOG
    && fs.readFileSync(process.env.FAKE_OPERATOR_IMPORT_LOG, 'utf8').trim().split('\n').length === Number(process.env.FAKE_OPERATOR_FAIL_CHUNK))) process.exit(2);
  console.log(JSON.stringify({ project, inserted, duplicates, conflicts, rejected, remaining }));
} else process.exit(2);
