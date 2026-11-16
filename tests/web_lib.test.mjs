import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile, readdir } from 'node:fs/promises';
import { squarify } from '../runtime/web/lib/treemap.mjs';
import { parse, href } from '../runtime/web/lib/router.mjs';
import { clip, fmtDay } from '../runtime/web/lib/format.mjs';
import { createApi, NOT_CONFIRMED } from '../runtime/web/lib/api.mjs';
import { needsYou, chartData } from '../runtime/web/lib/map.mjs';

test('treemap preserves areas, bounds and input order', () => {
  const items = [{key:'a',value:41},{key:'b',value:23},{key:'c',value:7}];
  const rects = squarify(items, 800, 500);
  assert.deepEqual(rects.map(r => r.key), items.map(i => i.key));
  for (const [i,r] of rects.entries()) {
    assert.ok(r.x >= 0 && r.y >= 0 && r.x+r.w <= 800.00001 && r.y+r.h <= 500.00001);
    assert.ok(Math.abs(r.w*r.h/400000 - items[i].value/71) < .01);
  }
  assert.deepEqual(rects, squarify(items, 800, 500));
});
test('treemap allocates finite minimum shares to empty roots', () => {
  const r = squarify([{key:'a',value:0},{key:'b',value:100}], 100, 100);
  assert.ok(r.every(x => Object.values(x).slice(1).every(Number.isFinite)));
  assert.ok(r[0].w*r[0].h >= 400-1e-8);
  assert.ok(squarify([{key:'a',value:0},{key:'b',value:0}],100,100).every(x => x.w*x.h > 0));
});
test('routes round trip including encoded projects and ids', () => {
  for (const project of ['@user','project / ü']) for (const page of ['map','type','memory','flat','flat-memory','skills','review','timeline','entry','activity']) {
    const hasId = ['type','memory','flat','flat-memory','entry'].includes(page);
    const route = {page, project, id:hasId ? 'id / 🌱' : undefined, state:page === 'review' ? 'conflicted' : undefined};
    assert.deepEqual(parse(href(route)), route);
  }
  assert.deepEqual(parse(href({page:'projects'})), {page:'projects'});
});
test('clip respects UTF-8 boundaries and fmtDay uses its date', () => {
  for (let n=0;n<16;n++) { const s=clip('a🌱é日', n); assert.ok(!s.includes('�')); assert.ok(Buffer.byteLength(s)<=n); assert.ok('a🌱é日'.startsWith(s)); }
  assert.equal(fmtDay('2026-11-15'), new Date('2026-11-15T12:00:00').toLocaleDateString(undefined,{month:'short',day:'numeric'}));
});
test('mobile tiles reset treemap coordinates and clip long Unicode titles', async () => {
  const css=await readFile('runtime/web/views/map.css','utf8');
  assert.match(css, /@media\(max-width:640px\)/);
  assert.match(css, /left:auto!important;\s*top:auto!important;\s*width:auto!important;\s*height:auto!important/);
  assert.match(css, /overflow-wrap:anywhere/);
  const title='🌱漢字'.repeat(200); const clipped=clip(title,99);
  assert.ok(title.startsWith(clipped)); assert.ok(Buffer.byteLength(clipped)<=99); assert.ok(!clipped.includes('�'));
});
test('API sends CSRF, unwraps tools, and never retries failures', async () => {
  let calls=0, init; let status=200, body={body:{ok:true}};
  const api=createApi({csrf:'test',fetchImpl:async (_path,options)=>{calls++; init=options; return {ok:status===200,status,json:async()=>body};}});
  assert.deepEqual(await api.tool('demo','memory_status'),{ok:true});
  assert.equal(init.headers['x-lambda-memory-csrf'],'test'); assert.deepEqual(JSON.parse(init.body),{project:'demo',name:'memory_status',arguments:{}});
  for (const [code,data,kind] of [[400,{},'invalid'],[409,{needs_start:true},'needs_start'],[502,{},'unconfirmed'],[503,{},'busy']]) {
    status=code; body=data; const before=calls; await assert.rejects(api.api('/api/start',{}),e=>e.kind===kind && (code!==502 || e.message===NOT_CONFIRMED)); assert.equal(calls,before+1);
  }
  status=502; body={capture_saved:true};
  await assert.rejects(api.api('/api/correct',{}),e=>e.kind==='unconfirmed' && /Evidence was saved/.test(e.message));
});
test('needs-you links target their exact queues', () => {
  const rows=needsYou({skill_drafts:{drafted:3}},{review:{unresolved_conflicts:2},lifecycle:{processing:4},forwards_failed:1});
  assert.deepEqual(rows.map(row=>href({project:'a / b',...row.route})), ['#/p/a%20%2F%20b/skills','#/p/a%20%2F%20b/review/conflicted','#/p/a%20%2F%20b/review/processing','#/p/a%20%2F%20b/activity']);
  assert.deepEqual(rows.map(row=>row.count),[3,2,4,1]);
});
test('chart summarizes both series, including empty and zero days', () => {
  const result=chartData([{day:'2026-11-15',captured:12,published:4},{day:'2026-11-16',captured:0,published:5}]);
  assert.equal(result.captured,12); assert.equal(result.published,9); assert.equal(result.max,12);
  assert.equal(result.days[0].tooltip,'15 Nov · 12 captured · 4 published');
  assert.equal(result.days[1].capturedHeight,0); assert.equal(chartData([]).max,0); assert.equal(chartData([{day:'2026-11-15',captured:0,published:0}]).days[0].publishedHeight,0);
});
test('status fixtures match diagnostic transport field types', async () => {
  const number=v=>assert.ok(Number.isInteger(v) && v>=0); const str=v=>assert.equal(typeof v,'string');
  const nullable=(v,check)=>{if (v!==null) check(v);};
  for (const name of await readdir('tests/fixtures/ui/memory_status')) {
    const fixture=JSON.parse(await readFile(`tests/fixtures/ui/memory_status/${name}`,'utf8')); if (fixture.status && fixture.status!==200) continue;
    const s=fixture.result.body; str(s.project); str(s.mode); number(s.observed_at_ms); number(s.forwards_pending); number(s.forwards_failed);
    str(s.worker.instance); number(s.worker.started_at_ms); number(s.worker.foreground_active);
    const d=s.worker.dreaming; str(d.state); nullable(d.phase,str); nullable(d.stop_reason,str); for (const k of ['idle_ms','tick_ms','dream_batch']) number(d[k]); nullable(d.remaining_idle_ms,number);
    for (const k of ['unconsumed_sources','processing_revisions','blocked_sources','blocked_revisions']) number(s.pending[k]); nullable(s.pending.active_without_current_embedding,number);
    str(s.tree.mode); for (const k of ['nodes_active','stale','split_queue','parked_sources']) number(s.tree[k]);
    for (const k of ['active','processing','conflicted','rejected','superseded']) number(s.lifecycle[k]); assert.equal(typeof s.review.activation_requires_review,'boolean'); number(s.review.unresolved_conflicts);
    for (const k of ['last_success','last_failure','last_dream_success','last_dream_failure']) nullable(s[k],entry=>{ for (const field of ['id','entry_kind','operation','phase','worker_instance','outcome']) str(entry[field]); for (const field of ['parent_id','reason']) nullable(entry[field],str); number(entry.started_at_ms); for (const field of ['finished_at_ms','classified_at_ms','duration_ms']) nullable(entry[field],number); assert.ok(entry.counts && typeof entry.counts==='object'); Object.values(entry.counts).forEach(number); });
    str(s.diagnostics.state); nullable(s.diagnostics.reason,str); number(s.history.retention_days); nullable(s.history.instrumentation_started_at_ms,number); number(s.history.retained_since_ms);
  }
});
test('web and screenshot sources have lines at most 110 characters', async () => {
  async function check(path) {
    const lines = (await readFile(path, 'utf8')).split('\n');
    lines.forEach((line, index) => assert.ok(line.length <= 110, `${path}:${index + 1} has ${line.length} characters`));
  }
  async function visit(dir) {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      if (entry.name === 'vendor' || entry.name === 'fonts') continue;
      const path = `${dir}/${entry.name}`;
      if (entry.isDirectory()) await visit(path);
      else await check(path);
    }
  }
  await visit('runtime/web');
  for (const name of await readdir('scripts')) {
    if (/^ui-.*\.mjs$/.test(name)) await check(`scripts/${name}`);
  }
});
test('runtime sources obey CSP restrictions and add no fonts', async () => {
  async function visit(dir) { for (const entry of await readdir(dir,{withFileTypes:true})) { if (entry.name === 'vendor' || entry.name === 'fonts') continue; const path=`${dir}/${entry.name}`; if (entry.isDirectory()) await visit(path); else { assert.ok(!/\.(woff2?|ttf|otf)$/i.test(path)); const text=await readFile(path,'utf8'); if (/\.(js|mjs)$/.test(path)) assert.doesNotMatch(text,/\.innerHTML|dangerouslySetInnerHTML/); if (path.endsWith('.css')) { assert.doesNotMatch(text,/@import/i); for (const m of text.matchAll(/url\s*\(([^)]*)\)/gi)) assert.match(m[1],/^\/fonts\/[a-z0-9-]+\.woff2$/); } } } }
  await visit('runtime/web');
});
