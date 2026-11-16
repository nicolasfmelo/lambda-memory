import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, existsSync, rmSync, statSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyId } from '../runtime/cross-store.mjs';

const script = resolve('runtime/promote-all.mjs');
const hash = p => createHash('sha256').update(p).digest('hex');
function fixture(t, options = {}) {
  const root = mkdtempSync(join(tmpdir(), 'lm-promote-')); t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root,'bin'), data = join(root,'data'); mkdirSync(bin); mkdirSync(data);
  const projects = [
    { project:'path:a',cwd:join(root,'one'),last_seen_ms:1 },
    { project:'path:b',cwd:join(root,'missing'),last_seen_ms:1 },
    { project:'path:c',cwd:join(root,'three'),last_seen_ms:1 },
    { project:'user:owner',cwd:data,last_seen_ms:1 },
    { project:'path:nostore',cwd:root,last_seen_ms:1 },
  ];
  mkdirSync(projects[0].cwd); mkdirSync(projects[2].cwd);
  for (const p of projects.slice(0,4)) mkdirSync(join(data,hash(p.project),'store'),{recursive:true});
  writeFileSync(join(data,'projects.json'),JSON.stringify({projects}),{mode:0o600});
  writeFileSync(join(bin,'systemctl'),`#!/bin/sh\necho ${options.active ? 'active' : 'inactive'}\n`,{mode:0o755});
  const statePath = join(root,'state.json'), log = join(root,'calls.jsonl');
  const state = { batches:{'path:a':2,'path:b':0,'path:c':1}, ...options }; writeFileSync(statePath,JSON.stringify(state));
  const native = join(bin,'native');
  writeFileSync(native,`#!/usr/bin/env node
const fs=require('node:fs'), path=require('node:path'),crypto=require('node:crypto');
const root=path.join(__dirname,'..'), state=JSON.parse(fs.readFileSync(path.join(root,'state.json'),'utf8'));
const project=process.env.LAMBDA_MEMORY_PROJECT, flag=process.argv[2], input=fs.readFileSync(0,'utf8');
fs.appendFileSync(path.join(root,'calls.jsonl'),JSON.stringify({project,flag,args:process.argv.slice(2),cwd:process.cwd(),store:process.env.LAMBDA_MEMORY_DATA_DIR,input})+'\\n');
const store=path.join(root,'data',crypto.createHash('sha256').update(project).digest('hex'),'store');
if(process.env.LAMBDA_MEMORY_DATA_DIR!==store) process.exit(3);
if(flag==='--inspect-promotions') console.log(JSON.stringify({project,nodes:['n1','n2'],batches:state.batches[project],linked_sources:3,accept_charges:false}));
else if(flag==='--promote-existing') {if(state.fail===project) process.exit(2);console.log(JSON.stringify({project,outcome:'succeeded',classified:2,user_nodes:['n1'],forwards:3}));}
else if(flag==='--export-forwards') {
  if(state.exportFail===project) process.exit(2);
  const count=project==='path:a' ? (state.forwardCount??137) : 0;
  for(let i=0;i<count;i++) console.log(JSON.stringify({forward_id:state.forwardId ?? 'capture:s'+i,source_id:state.sourceId ?? 's'+i,source:{source:'s'+i,origin:'original://source',recorded_at_ms:123,kind:'decision',outcome:'unknown',content:'Delegate only coding tasks.'}}));
} else if(flag==='--import-sources') {
  if(state.importFail) process.exit(2);
  const rows=input.trim().split('\\n').filter(Boolean).map(JSON.parse), inserted_ids=[],duplicate_ids=[],remaining_ids=[],rejected=[],conflicts=[];
  for(const row of rows) {const n=Number(row.source.split('/').at(-1).slice(1)); (n===129?rejected:n===130?conflicts:n===131?remaining_ids:n%2?duplicate_ids:inserted_ids).push(row.source);}
  console.log(JSON.stringify({project,inserted:inserted_ids.length,duplicates:duplicate_ids.length,remaining:remaining_ids.length,inserted_ids,duplicate_ids,remaining_ids,rejected,conflicts}));
} else if(flag==='--forward-done') {const r=JSON.parse(input);console.log(JSON.stringify({done:r.done.length,failed:r.failed.length}));}
else if(flag==='--retire-promoted') {
  if(process.argv.includes('--apply')) {const ids=JSON.parse(input);console.log(JSON.stringify({project,retired:ids.length,eligible:[],blocked:[]}));}
  else console.log(JSON.stringify({project,eligible:[
    {node:'z-confirmed',sources:['active','consumed']},{node:'a-confirmed',sources:['active']},
    {node:'waiting',sources:['pending','missing','active']},{node:'incomplete',sources:['active','missing']},
    {node:'orphaned',sources:['consumed']},{node:'blocked',sources:['pending']}],blocked:['blocked'],retired:0}));
} else if(flag==='--inspect-copies') {
  const result={active:[],consumed:[],pending:[],missing:[]};for(const id of JSON.parse(input)) result[id.split('/').at(-1)].push(id);console.log(JSON.stringify(result));
} else process.exit(4);
`,{mode:0o755});
  const config = join(root,'config.json'); writeFileSync(config,JSON.stringify({nativeBin:native,bridgeBin:native,dataDir:data,socketPath:join(root,'socket'),owner:'owner',actor:'operator',token:'x'.repeat(40)}),{mode:0o600});
  const env = {...process.env,PATH:`${bin}:${process.env.PATH}`,LAMBDA_MEMORY_CONFIG:config};
  const run = (action,input,args=[]) => spawnSync(process.execPath,[script,action,...args],{env,input:input===undefined ? undefined : JSON.stringify(input),encoding:'utf8'});
  const calls = () => existsSync(log) ? readFileSync(log,'utf8').trim().split('\n').filter(Boolean).map(JSON.parse) : [];
  const update = values => {Object.assign(state,values);writeFileSync(statePath,JSON.stringify(state));};
  return {root,data,projects,run,calls,update,env};
}
function json(result) { assert.equal(result.status,0,result.stderr); return JSON.parse(result.stdout); }

test('promotion plan includes missing-directory entries, skips only user and missing stores, cheapest first',t => {
  const f=fixture(t),plan=json(f.run('plan'));
  assert.deepEqual(plan.projects.map(p=>p.project),['path:b','path:c','path:a']); assert.equal(plan.total_batches,3); assert.equal(plan.total_linked_sources,9); assert.equal(plan.accept_charges,false);
  assert.deepEqual(plan.skipped,[{project:'path:nostore',reason:'missing_store'},{project:'user:owner',reason:'user_store'}]);
  assert.equal(f.calls().find(c=>c.project==='path:b').cwd,f.data);
  assert.equal(f.calls().find(c=>c.project==='path:a').cwd,f.projects[0].cwd);
});
test('promotion summary explains downstream curation and embeddings costs',t => {
  const f=fixture(t),result=f.run('summary',json(f.run('plan'))); assert.equal(result.status,0,result.stderr);
  assert.match(result.stdout,/user-store curation and embeddings/); assert.match(result.stdout,/upper bound.*9/); assert.match(result.stdout,/paid parent refreshes/); assert.match(result.stdout,/Consent: not given/);
});
test('promotion apply requires the exact fresh plan and true consent',t => {
  const f=fixture(t),plan=json(f.run('plan'));
  assert.equal(f.run('apply',plan).status,2);
  plan.accept_charges=true; plan.projects[0].nodes.push('changed'); assert.equal(f.run('apply',plan).status,2);
  assert.equal(f.calls().filter(c=>c.flag==='--promote-existing').length,0);
  const current=json(f.run('plan'));current.accept_charges=true;f.update({batches:{'path:a':3,'path:b':0,'path:c':1}});
  assert.equal(f.run('apply',current).status,2);
});
test('promotion apply stops at first failure and never runs later projects',t => {
  const f=fixture(t,{fail:'path:c'}),plan=json(f.run('plan'));plan.accept_charges=true;
  const result=f.run('apply',plan);assert.equal(result.status,2,result.stderr);
  assert.deepEqual(JSON.parse(result.stdout).outcomes.map(x=>x.outcome),['succeeded','failed']);
  assert.deepEqual(f.calls().filter(c=>c.flag==='--promote-existing').map(c=>c.project),['path:b','path:c']);
  for(const c of f.calls().filter(c=>c.flag==='--promote-existing')) assert.equal(JSON.parse(c.input).accept_charges,true);
});
test('transfer rewrites copy identities, preserves evidence and acks done/failed in chunks of 64 leaving remaining pending',t => {
  const f=fixture(t),report=json(f.run('transfer'));
  assert.equal(report.outcomes[0].done,134);assert.equal(report.outcomes[0].failed,2);assert.equal(report.outcomes[0].remaining,1);
  const imports=f.calls().filter(c=>c.flag==='--import-sources');assert.equal(imports.length,1);assert.equal(imports[0].project,'user:owner');
  const rows=imports[0].input.trim().split('\n').map(JSON.parse);assert.equal(rows.length,137);
  assert.deepEqual(rows[0],{source:copyId('path:a','s0'),origin:'promote://path:a/s0',recorded_at_ms:123,kind:'decision',outcome:'unknown',content:'Delegate only coding tasks.'});
  const acks=f.calls().filter(c=>c.flag==='--forward-done').map(c=>JSON.parse(c.input));assert.equal(acks.length,3);
  assert.deepEqual(acks.map(a=>a.done.length+a.failed.length),[64,64,8]);
  assert.deepEqual(acks.flatMap(a=>a.failed),['capture:s129','capture:s130']);
  assert.ok(!acks.flatMap(a=>[...a.done,...a.failed]).includes('capture:s131'));
  assert.equal(json(f.run('transfer')).outcomes[0].remaining,1);
});
test('transfer accepts and acknowledges generation IDs but rejects another source',t => {
  const f=fixture(t,{forwardCount:1,forwardId:'recapture:123:s0'});
  json(f.run('transfer'));
  assert.deepEqual(JSON.parse(f.calls().find(c=>c.flag==='--forward-done').input).done,['recapture:123:s0']);
  f.update({sourceId:'s0:part',forwardId:'recapture:456:s0:part'});
  json(f.run('transfer'));
  assert.deepEqual(JSON.parse(f.calls().filter(c=>c.flag==='--forward-done').at(-1).input).done,['recapture:456:s0:part']);
  f.update({sourceId:'s0'});
  for (const forwardId of ['capture:s1', 'capture:s0:123', 'recapture:123:s1', 'recapture::s0', 'recapture:x:s0', 'recapture:123:s0:extra', 'recapture:123s0']) {
    f.update({forwardId});
    const refused=f.run('transfer'); assert.equal(refused.status,2,forwardId); assert.match(refused.stderr,/invalid capture forward/);
  }
});
test('transfer creates a private missing user store parent',t => {
  const f=fixture(t,{forwardCount:1}); const dir=join(f.data,hash('user:owner'));
  rmSync(dir,{recursive:true});
  const old=process.umask(0o022); try { json(f.run('transfer')); } finally { process.umask(old); }
  assert.equal(statSync(dir).mode & 0o777,0o700);
});
test('transfer chunks imports and stops before acknowledgement on native failure',t => {
  const f=fixture(t);
  const wrapper=`const {main}=await import(${JSON.stringify(new URL('../runtime/promote-all.mjs',import.meta.url).href)});process.argv=[process.execPath,${JSON.stringify(script)},'transfer'];await main({chunkLines:70});`;
  const result=spawnSync(process.execPath,['--input-type=module','-e',wrapper],{env:f.env,encoding:'utf8'});assert.equal(result.status,0,result.stderr);
  assert.deepEqual(f.calls().filter(c=>c.flag==='--import-sources').map(c=>c.input.trim().split('\n').length),[70,67]);
  const before=f.calls().filter(c=>c.flag==='--forward-done').length;f.update({importFail:true});assert.equal(f.run('transfer').status,2);
  assert.equal(f.calls().filter(c=>c.flag==='--forward-done').length,before);
});
test('retire-plan precedence sorts confirmed, waiting, incomplete, orphaned and blocked',t => {
  const f=fixture(t),plan=json(f.run('retire-plan'));
  assert.equal(plan.accept_charges,false); assert.equal(plan.projects.length,3);
  assert.deepEqual(plan.projects[0],{project:'path:a',confirmed:['a-confirmed','z-confirmed'],waiting:['waiting'],incomplete:['incomplete'],orphaned:['orphaned'],blocked:['blocked']});
  const inspections=f.calls().filter(c=>c.flag==='--inspect-copies');assert.equal(inspections.length,3);
  assert.ok(inspections.every(c=>c.project==='user:owner'&&JSON.parse(c.input).length<=10000));
});
test('retire requires --confirm and exact consent, then applies only confirmed nodes',t => {
  const f=fixture(t),plan=json(f.run('retire-plan'));
  assert.equal(f.run('retire',plan).status,2);assert.equal(f.run('retire',plan,['--confirm']).status,2);
  plan.accept_charges=true;plan.projects[0].confirmed.push('incomplete');assert.equal(f.run('retire',plan,['--confirm']).status,2);
  assert.equal(f.calls().filter(c=>c.args.includes('--apply')).length,0);
  const current=json(f.run('retire-plan'));current.accept_charges=true;
  const result=json(f.run('retire',current,['--confirm']));assert.deepEqual(result.outcomes.map(p=>p.retired),[2,2,2]);
  for(const c of f.calls().filter(c=>c.args.includes('--apply'))) assert.deepEqual(JSON.parse(c.input),['a-confirmed','z-confirmed']);
});
test('every promotion action refuses while service is active',t => {
  const f=fixture(t,{active:true});
  for(const action of ['plan','summary','apply','transfer','retire-plan','retire']) {
    const result=f.run(action,{},action==='retire'?['--confirm']:[]);assert.equal(result.status,2);assert.match(result.stderr,/service to be stopped/);
  }
  assert.equal(f.calls().length,0);
});
