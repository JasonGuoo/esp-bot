import assert from "node:assert/strict";
import test from "node:test";
import {AppearanceSettings} from "../settings-model.js";
import {renderFace, hasFaceRenderer} from "../face-renderers.js";

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
