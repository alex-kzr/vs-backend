#!/usr/bin/env node
// Local transport only. Semantics, browser access and model execution belong to the explicitly requested external agent.
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';
import { pathToFileURL } from 'node:url';

export function localBase(raw='http://127.0.0.1:48173') {
 const u=new URL(raw);
 if(!['http:','https:'].includes(u.protocol)||!['127.0.0.1','localhost','[::1]'].includes(u.hostname)||u.username||u.password||u.pathname!=='/'||u.search||u.hash)
  throw new Error('Only an explicit loopback origin is allowed.');
 return u.origin;
}
const uuid='[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}';
const postPath=new RegExp('^(?:cvs(?:/import-preview|/'+uuid+'/revisions)?|match-runs(?:/preview|/'+uuid+'/cancel|/'+uuid+'/jobs/'+uuid+'/(?:claim|complete|fail|retry))?)$');
export function workflowPath(path) {if(!postPath.test(path))throw new Error('Only documented CV / matching POST paths are allowed.');return path;}
async function request(base,path,method,body,key) {
 const r=await fetch(base+'/api/v1/'+path,{method,redirect:'error',signal:AbortSignal.timeout(30000),headers:method==='POST'?{'Content-Type':'application/json','Idempotency-Key':key}:{},...(method==='POST'?{body:JSON.stringify(body)}:{})});
 const raw=await r.text();if(Buffer.byteLength(raw)>32*1024*1024)throw new Error('Local response exceeds 32 MiB.');
 let result;try{result=JSON.parse(raw);}catch{throw new Error('Local API returned invalid JSON; retry the same envelope.');}
 if(!r.ok)throw new Error('Local API rejected request: HTTP '+r.status+', '+(typeof result?.error?.code==='string'?result.error.code:'invalid_error')+'.');
 return result;
}
async function save(path,data,exclusive=false) {
 await mkdir(dirname(path),{recursive:true});await writeFile(path,JSON.stringify(data,null,2)+'\n',{encoding:'utf8',mode:0o600,flag:exclusive?'wx':'w'});
}
export async function postWorkflow({baseUrl,path,input,out}) {
 const base=localBase(baseUrl);workflowPath(path);
 if(!input||Array.isArray(input)||typeof input!=='object')throw new Error('Input must be a JSON object.');
 if(Buffer.byteLength(JSON.stringify(input))>1024*1024)throw new Error('Input exceeds the API 1 MiB request limit.');
 const output=resolve(out);const envelopePath=output+'.request.json';
 const identity={schema_version:1,base,path,input};
 let envelope;
 try{envelope=JSON.parse(await readFile(envelopePath,'utf8'));}
 catch(e){if(e.code!=='ENOENT')throw new Error('Existing request envelope cannot be read.');envelope={...identity,key:randomUUID()};await save(envelopePath,envelope,true);}
 const {key,...previous}=envelope;
 if(!isDeepStrictEqual(previous,identity)||typeof key!=='string'||!new RegExp('^'+uuid+'$').test(key))
  throw new Error('Output belongs to a different request. Reuse unchanged inputs or choose a new output filename.');
 // Persisting the UUID BEFORE POST lets an agent recover a response lost after a committed write.
 const receipt=await request(base,path,'POST',input,key);await save(output,receipt);return receipt;
}
export async function exportRun({baseUrl,run,out}) {
 if(!new RegExp('^'+uuid+'$').test(run))throw new Error('Run must be a UUID.');
 const result=await request(localBase(baseUrl),'match-runs/'+run+'/inputs','GET');
 await save(resolve(out),result);return result;
}
async function main(args) {
 const command=args.shift();const options={};
 while(args.length){const name=args.shift();if(!/^--(?:base-url|path|input|out|run)$/.test(name)||!args.length||Object.hasOwn(options,name))throw new Error('Invalid command options.');options[name]=args.shift();}
 if(!options['--out'])throw new Error('Choose a private local --out file, outside Git.');
 if(command==='export'&&options['--run']&&!options['--path']&&!options['--input'])
  await exportRun({baseUrl:options['--base-url'],run:options['--run'],out:options['--out']});
 else if(command==='post'&&options['--path']&&options['--input']&&!options['--run']){
  const inputText=await readFile(resolve(options['--input']),'utf8');
  if(Buffer.byteLength(inputText)>1024*1024)throw new Error('Input exceeds 1 MiB.');
  let input;try{input=JSON.parse(inputText);}catch{throw new Error('Input file is not JSON.');}
  await postWorkflow({baseUrl:options['--base-url'],path:options['--path'],input,out:options['--out']});
 }else throw new Error('Use export --run UUID --out file, or post --path endpoint --input file --out receipt.');
 process.stdout.write('Local file saved. No model or browser was invoked.\n');
}
if(process.argv[1]&&pathToFileURL(resolve(process.argv[1])).href===import.meta.url)
 main(process.argv.slice(2)).catch(e=>{process.stderr.write(e instanceof Error?e.message+'\n':'Local transport failed.\n');process.exitCode=1;});
