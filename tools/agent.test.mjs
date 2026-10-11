import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { localBase, workflowPath, postWorkflow, exportRun } from './agent.mjs';

test('only loopback origins and documented workflows are accepted',()=>{
 assert.equal(localBase(),'http://127.0.0.1:48173');
 for(const origin of ['https://example.com','http://user:pass@localhost','http://127.0.0.1/api','http://127.0.0.1/?token=x'])assert.throws(()=>localBase(origin));
 for(const path of ['../vacancies','vacancies','cvs?x=y','cvs/../../companies'])assert.throws(()=>workflowPath(path));
 assert.equal(workflowPath('cvs/import-preview'),'cvs/import-preview');
});
test('lost response reuses persisted request UUID and changed input is blocked',async()=>{
 const dir=await mkdtemp(join(tmpdir(),'vacancy-agent-'));const keys=[];let requests=0;
 const server=createServer(async(req,res)=>{
  for await(const _chunk of req){ /* Consume synthetic body without logging it. */ }
  keys.push(req.headers['idempotency-key']);requests++;
  if(requests===1){res.destroy();return;}
  res.setHeader('content-type','application/json');res.end(JSON.stringify({data:{id:'synthetic'},deduplicated:true}));
 });
 await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
 try{
  const opts={baseUrl:'http://127.0.0.1:'+server.address().port,path:'cvs',input:{content:'Synthetic Rust CV'},out:join(dir,'receipt.json')};
  await assert.rejects(postWorkflow(opts));const envelope=JSON.parse(await readFile(opts.out+'.request.json','utf8'));assert.ok(envelope.key);
  assert.equal((await postWorkflow(opts)).deduplicated,true);assert.equal(keys[0],keys[1]);
  await assert.rejects(postWorkflow({...opts,input:{content:'Changed'}}),/different request/);assert.equal(requests,2);
 }finally{await new Promise(resolve=>server.close(resolve));await rm(dir,{recursive:true,force:true});}
});
test('export reads pinned packet without claiming jobs or invoking a model',async()=>{
 const dir=await mkdtemp(join(tmpdir(),'vacancy-export-'));const calls=[];const run='10000000-0000-4000-8000-000000000001';
 const server=createServer((req,res)=>{calls.push([req.method,req.url]);res.setHeader('content-type','application/json');res.end(JSON.stringify({data:{schema_version:1,cv_revision:{content:'Synthetic'},jobs:[{state:'pending'}]}}));});
 await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
 try{
  const out=join(dir,'packet.json');await exportRun({baseUrl:'http://127.0.0.1:'+server.address().port,run,out});
  assert.equal(JSON.parse(await readFile(out,'utf8')).data.jobs[0].state,'pending');
  assert.deepEqual(calls,[['GET','/api/v1/match-runs/'+run+'/inputs']]);
 }finally{await new Promise(resolve=>server.close(resolve));await rm(dir,{recursive:true,force:true});}
});
