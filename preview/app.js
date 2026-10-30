import {renderFace,hasFaceRenderer} from "./face-renderers.js";
import {AppearanceSettings} from "./settings-model.js";

const $=id=>document.getElementById(id);
const EXPRESSIONS=[
  ["idle","待机"],["listening","倾听"],["happy","开心"],
  ["curious","好奇"],["thinking","思考"],["surprised","惊讶"],
  ["sleepy","困倦"],["shy","害羞"],["blink","眨眼"]
];
const KEY="espbot.appearance.v1";
let themes=[],model=null,expression="idle",gaze={x:0,y:0};
const byId=id=>themes.find(t=>t.id===id);
function safeRead(){try{return localStorage.getItem(KEY)}catch{return null}}
function safeSave(id){try{localStorage.setItem(KEY,id)}catch{/* privacy mode */}}

function updateScreen(){
  if(!model)return;
  const t=byId(model.activeId);
  const screen=$("bot-screen");
  screen.style.setProperty("--bg",t.background);
  screen.style.setProperty("--fg",t.eye);
  screen.style.setProperty("--text",t.subtitle_text);
  screen.style.setProperty("--surface",t.subtitle_bg);
  screen.style.setProperty("--chrome",t.chrome);
  $("face-graphics").innerHTML=renderFace(t,expression,gaze);

  const subtitle=$("subtitle-input").value.trim();
  $("subtitle-bar").hidden=!subtitle;
  $("subtitle-output").textContent=subtitle;
  $("gear").hidden=!(model.page==="face" && model.gearVisible);
  $("settings-screen").hidden=model.page==="face";
  $("settings-home").hidden=model.page!=="settings";
  $("appearance-page").hidden=model.page!=="appearance";
  $("active-color-dot").style.background=t.eye;
  document.querySelectorAll("[data-appearance]").forEach(card=>{
    card.setAttribute("aria-pressed",String(card.dataset.appearance===model.selectedId));
  });
  document.querySelectorAll("[data-expression]").forEach(button=>{
    button.setAttribute("aria-pressed",String(button.dataset.expression===expression));
  });
}

function buildThemeOptions(){
  const list=$("appearance-grid");list.replaceChildren();
  for(const t of themes){
    const card=document.createElement("button");
    card.className="appearance-card";
    card.type="button";card.dataset.appearance=t.id;
    card.style.setProperty("--card-eye",t.eye);
    card.style.setProperty("--card-bg",t.background);
    card.setAttribute("aria-label","选择"+t.name_zh+"外观");
    const thumb=document.createElement("span");
    thumb.className="appearance-thumb";
    const svg=document.createElementNS("http://www.w3.org/2000/svg","svg");
    svg.setAttribute("viewBox","0 0 800 480");
    svg.setAttribute("aria-hidden","true");
    const g=document.createElementNS("http://www.w3.org/2000/svg","g");
    g.innerHTML=renderFace(t,"idle",{x:0,y:0});
    svg.appendChild(g);thumb.appendChild(svg);
    const name=document.createElement("span");name.className="card-name";
    name.textContent=t.name_zh;
    const subtitle=document.createElement("span");subtitle.className="card-sub";
    subtitle.textContent=t.name;
    card.append(thumb,name,subtitle);
    card.addEventListener("click",e=>{
      e.stopPropagation();
      model.select(t.id);
      updateScreen();
    });
    list.appendChild(card);
  }
}
function buildExpressionOptions(){
  const list=$("expressions");list.replaceChildren();
  for(const [id,label] of EXPRESSIONS){
    const b=document.createElement("button");
    b.type="button";b.dataset.expression=id;b.textContent=label;
    b.addEventListener("click",()=>{expression=id;updateScreen()});
    list.appendChild(b);
  }
}

function connectEvents(){
  $("bot-screen").addEventListener("click",e=>{
    if(!$("settings-screen").hidden || $("gear").contains(e.target))return;
    model.tapFace(Date.now());
    updateScreen();
  });
  $("gear").addEventListener("click",e=>{
    e.stopPropagation();
    model.openSettings();
    updateScreen();
  });
  $("settings-close").addEventListener("click",()=>{
    model.close();updateScreen();
  });
  $("appearance-open").addEventListener("click",()=>{
    model.openAppearance();updateScreen();
  });
  $("appearance-back").addEventListener("click",()=>{
    model.back();updateScreen();
  });
  $("appearance-cancel").addEventListener("click",()=>{
    model.back();updateScreen();
  });
  $("appearance-apply").addEventListener("click",()=>{
    const id=model.apply();
    if(id!==null)safeSave(id);
    updateScreen(); /* settings gone, redraw face with newly chosen renderer */
  });
  for(const axis of ["x","y"]){
    $("gaze-"+axis).addEventListener("input",e=>{
      gaze[axis]=Number(e.target.value);
      $("gaze-"+axis+"-number").textContent=gaze[axis];
      updateScreen();
    });
  }
  $("center-gaze").addEventListener("click",()=>{
    gaze={x:0,y:0};
    for(const axis of ["x","y"]){
      $("gaze-"+axis).value="0";
      $("gaze-"+axis+"-number").textContent="0";
    }
    updateScreen();
  });
  $("subtitle-input").addEventListener("input",updateScreen);
  $("subtitle-demo").addEventListener("click",()=>{
    $("subtitle-input").value="你好，我正在看着你。";
    expression="happy";updateScreen();
  });
  $("subtitle-clear").addEventListener("click",()=>{
    $("subtitle-input").value="";updateScreen();
  });
  setInterval(()=>{
    if(!model || !model.gearVisible)return;
    const before=model.gearVisible;
    model.tick(Date.now());
    if(before!==model.gearVisible)updateScreen();
  },200);
}

async function init(){
  try{
    const response=await fetch("../shared/themes.json",{cache:"no-cache"});
    if(!response.ok)throw new Error("主题文件读取失败 (HTTP "+response.status+")");
    const manifest=await response.json();
    if(manifest.schema_version!==1 || !Array.isArray(manifest.themes) ||
       !manifest.themes.length)throw new Error("外观清单无效");
    themes=manifest.themes;
    for(const t of themes){
      if(!hasFaceRenderer(t.renderer))
        throw new Error("缺少外观渲染器 "+t.renderer+" — "+t.id);
    }
    const stored=safeRead();
    const first=themes.find(t=>t.id===stored)?.id ??
       themes.find(t=>t.id===manifest.default_theme)?.id ?? themes[0].id;
    model=new AppearanceSettings(themes.map(t=>t.id),first);
    buildThemeOptions();
    buildExpressionOptions();
    connectEvents();
    updateScreen();
  }catch(error){
    $("error").hidden=false;
    $("error").textContent="预览未能启动："+error.message+
      "。请从仓库根目录运行 python3 -m http.server 8765，并访问 /preview/。";
  }
}
init();
