// ROM-free opt-in identity checks against the actual compiled WASM ABI.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import {createJitHost} from '../../web/wasm/jit.mjs';
let w;
const host = createJitHost(() => w);
w = (await WebAssembly.instantiate(await fs.readFile(process.argv[2]), {env: {...host.imports, host_log() {}, host_profile_now: () => performance.now()}})).instance.exports;
function buffer(data, fn) {const p=w.esp32sim_alloc(data.length);new Uint8Array(w.memory.buffer).set(data,p);try{return fn(p);}finally{w.esp32sim_free(p,data.length);}}
for (const board of ['x4pro','esp32c3']) {
 const e=buffer(new TextEncoder().encode(board),p=>w.esp32sim_new(p,board.length,4,0));assert.notEqual(e,0);
 try {
  assert.equal(w.esp32sim_set_factory_mac(e,0,6),1);
  for(const mac of [[0,0,0,0,0,0],[255,255,255,255,255,255],[3,1,2,3,4,5],[0,1,2,3,4,5]])buffer(new Uint8Array(mac),p=>assert.equal(w.esp32sim_set_factory_mac(e,p,6),1));
  buffer(new Uint8Array([2,1,2,3,4,5]),p=>{
   assert.equal(w.esp32sim_set_factory_mac(e,p,5),1);
   assert.equal(w.esp32sim_set_factory_mac(e,p,6),board==='x4pro'?0:3);
   assert.equal(w.esp32sim_boot(e,0),0);
   assert.equal(w.esp32sim_set_factory_mac(e,p,6),2);
  });
 }finally{w.esp32sim_delete(e);}
}
console.log('PASS actual WASM factory MAC ABI: opt-in S3, invalid/unsupported/late refusal');
