import assert from "node:assert/strict";
import test from "node:test";
import {AppearanceSettings} from "../settings-model.js";
import {renderFace, hasFaceRenderer, PREVIEW_EXPRESSIONS, FACE_EXPRESSIONS} from "../face-renderers.js";
import {readFileSync} from "node:fs";

test("gear stays hidden until tap, expires after five seconds",()=>{
 const m=new AppearanceSettings(["mint","sakura"],"mint");
 assert.equal(m.gearVisible,false);
 m.tapFace(1000);
 assert.equal(m.gearVisible,true);
 m.tick(5999);assert.equal(m.gearVisible,true);
 m.tick(6000);assert.equal(m.gearVisible,false);
 assert.equal(m.openSettings(),false);
});

test("Settings -> Appearance -> Select -> Apply -> Face",()=>{
 const m=new AppearanceSettings(["mint","sakura","sky"],"mint");
 m.tapFace(10);
 assert.equal(m.openSettings(),true);
 assert.equal(m.openAppearance(),true);
 assert.equal(m.select("sakura"),true);
 assert.equal(m.activeId,"mint","not applied yet");
 assert.equal(m.apply(),"sakura");
 assert.equal(m.activeId,"sakura");
 assert.equal(m.page,"face");
 assert.equal(m.gearVisible,false);
});

test("Back and close discard uncommitted appearance",()=>{
 const m=new AppearanceSettings(["mint","sakura"],"mint");
 m.tapFace(0);m.openSettings();m.openAppearance();m.select("sakura");
 assert.equal(m.back(),true);
 assert.equal(m.page,"settings");
 assert.equal(m.activeId,"mint");
 m.openAppearance();assert.equal(m.selectedId,"mint");
 m.select("sakura");m.close();
 assert.equal(m.activeId,"mint");
});

test("unknown or out-of-page selection is ignored",()=>{
 const m=new AppearanceSettings(["mint","sky"],"mint");
 assert.equal(m.select("sky"),false);
 m.tapFace(0);m.openSettings();m.openAppearance();
 assert.equal(m.select("missing"),false);
 assert.equal(m.activeId,"mint");
});

test("appearance renderer implementations are interchangeable",()=>{
 const base={eye:"#A8E8D6",background:"#142423"};
 const outline=renderFace({...base,renderer:"outline"},"idle",{x:20,y:0});
 const pill=renderFace({...base,renderer:"pill"},"idle",{x:20,y:0});
 assert.notEqual(outline,pill);
 assert.match(outline,/circle/);
 assert.doesNotMatch(pill,/circle/);
 assert.equal(hasFaceRenderer("outline"),true);
 assert.equal(hasFaceRenderer("pill"),true);
 assert.throws(()=>renderFace({...base,renderer:"unknown"}),/Missing appearance renderer/);
});

test("EVE is the default and original saved theme IDs stay selectable",()=>{
 const manifest=JSON.parse(readFileSync(new URL("../../shared/themes.json",import.meta.url)));
 assert.equal(manifest.default_theme,"eve");
 const ids=manifest.themes.map(t=>t.id);
 assert.deepEqual(ids,["eve","mint","sakura","lavender","butter","sky"]);
 for(const id of ids){
  const m=new AppearanceSettings(ids,id);
  m.tapFace(0);m.openSettings();m.openAppearance();
  m.select(id==="eve"?"mint":"eve");
  m.back();assert.equal(m.activeId,id);
 }
});

test("all EVE poses are distinct, have no pupils, and keep shared gaze",()=>{
 const t={renderer:"eve",eye:"#35D7FF",background:"#000000"};
 assert.equal(PREVIEW_EXPRESSIONS.length,24);
 const shapes=new Set();
 for(const {id} of PREVIEW_EXPRESSIONS){
  const svg=renderFace(t,id,{x:0,y:0});
  assert.doesNotMatch(svg,/circle|NaN|undefined/);
  shapes.add(svg.replaceAll(/eve-\d+/g,"eve"));
  const path=s=>[...s.matchAll(/<path d="([^"]+)"/g)].slice(0,2).map(m=>m[1].match(/-?\d+(?:\.\d+)?/g).map(Number));
  const neutral=path(svg), moved=path(renderFace(t,id,{x:75,y:-70}));
  for(let eye=0;eye<2;eye++)for(let p=0;p<neutral[eye].length;p+=2){
   assert.ok(Math.abs(moved[eye][p]-(neutral[eye][p]+69))<.03);
   assert.ok(Math.abs(moved[eye][p+1]-neutral[eye][p+1]+12)<.02);
  }
 }
 assert.equal(shapes.size,24);
 assert.equal(FACE_EXPRESSIONS.length,28,"includes the four lifecycle/animation compatibility poses");
 const first=renderFace(t), second=renderFace(t);
 assert.notEqual(first.match(/id="([^"]+)-glow/)[1],second.match(/id="([^"]+)-glow/)[1]);
});

test("side looks move both eyes together without changing their shapes",()=>{
 const t={renderer:"eve",eye:"#35D7FF",background:"#000000"};
 const paths=x=>[...renderFace(t,"idle",{x,y:0}).matchAll(/<path d="([^"]+)"/g)].slice(0,2)
  .map(m=>m[1].match(/-?\d+(?:\.\d+)?/g).map(Number).filter((_,i)=>i%2===0));
 const width=points=>Math.max(...points)-Math.min(...points);
 const left=paths(-100), right=paths(100), center=paths(0);
 for(let eye=0;eye<2;eye++) {
  assert.ok(Math.abs(width(left[eye])-width(center[eye]))<.01);
  assert.ok(Math.abs(width(right[eye])-width(center[eye]))<.01);
 }
 assert.ok(Math.abs(width(center[0])-width(center[1]))<.01);
 assert.ok(Math.min(...right[0])-Math.min(...left[0])>180);
 assert.ok(Math.min(...right[1])-Math.min(...left[1])>180);
});

test("eye reflections are reserved for exceptional joy",()=>{
 const t={renderer:"eve",eye:"#35D7FF",background:"#000000"};
 for(const {id} of FACE_EXPRESSIONS) {
  const glints=[...renderFace(t,id).matchAll(/data-highlight="joy"/g)];
  assert.equal(glints.length,id==="overjoyed" ? 2 : 0,id);
 }
});

test("both EVE eyes remain within the screen at the full gaze limits",()=>{
 const t={renderer:"eve",eye:"#35D7FF",background:"#000000"};
 for(const {id} of PREVIEW_EXPRESSIONS){
  for(const x of [-100,100]) for(const y of [-100,100]){
   const svg=renderFace(t,id,{x,y});
   const paths=[...svg.matchAll(/<path d="([^"]+)"/g)].slice(0,2);
   assert.equal(paths.length,2);
   for(const match of paths){
    const points=match[1].match(/-?\d+(?:\.\d+)?/g).map(Number);
    for(let i=0;i<points.length;i+=2){
     assert.ok(points[i]>=8 && points[i]<=792,`${id}: x=${points[i]}`);
     assert.ok(points[i+1]>=8 && points[i+1]<=380,`${id}: y=${points[i+1]}`);
    }
   }
  }
 }
});
