/*
 * Browser renderers are standalone visual adapters sharing the same theme
 * manifest as the native LVGL renderer registry. Other eye aesthetics can
 * register an implementation here without modifying settings or the app.
 */
import {EVE} from "./eve-expressions.generated.js";
export const FACE_EXPRESSIONS = EVE.expressions;
export const PREVIEW_EXPRESSIONS = EVE.preview_order.map(id => EVE.expressions.find(e => e.id === id));
const color = theme => theme.eye;
const legacyExpression = expression => ({
  gentle:"listening", confused:"curious", discovery:"surprised", laughing:"happy",
  excited:"happy", wink:"thinking", smug:"shy", skeptical:"thinking",
  determined:"listening", annoyed:"bored", worried:"shy"
}[expression] ?? expression);

function outlineEyes(theme, expression, gaze) {
  expression = legacyExpression(expression);
  const fg = color(theme), dx = gaze.x * .23, dy = gaze.y * .18;
  let left = "open", right = "open";
  let lw = 148, rw = 148, lh = 136, rh = 136, ly = 212, ry = 212;
  if (expression === "happy" || expression === "shy") left = right = "arch";
  if (["sleepy","sleeping","blink"].includes(expression)) left = right = "slit";
  if (expression === "bored") lh = rh = 88;
  if (expression === "drowsy") lh = rh = 54;
  if (expression === "curious") { rw = 124; rh = 104; ry += 11; }
  if (expression === "thinking") right = "slit";
  if (expression === "surprised") { lw = rw = 170; lh = rh = 156; }

  function eye(cx, cy, w, h, type) {
    if (type === "slit") {
      return '<path d="M ' + (cx-w*.37) + " " + (cy+10) +
        " Q " + cx + " " + (cy+17) + " " + (cx+w*.37) + " " + (cy+10) +
        '" fill="none" stroke="' + fg + '" stroke-width="17" stroke-linecap="round"/>';
    }
    if (type === "arch") {
      return '<path d="M ' + (cx-w*.43) + " " + (cy+25) +
        " Q " + cx + " " + (cy-h*.80) + " " + (cx+w*.43) + " " + (cy+25) +
        '" fill="none" stroke="' + fg + '" stroke-width="21" stroke-linecap="round"/>';
    }
    return '<rect x="' + (cx-w/2) + '" y="' + (cy-h/2) +
      '" width="' + w + '" height="' + h +
      '" rx="' + (Math.min(w,h)*.43) + '" fill="none" stroke="' + fg +
      '" stroke-width="21"/><circle cx="' + (cx+dx) + '" cy="' + (cy+dy) +
      '" r="12" fill="' + fg + '"/>';
  }
  const eyes = eye(255,ly,lw,lh,left) + eye(545,ry,rw,rh,right);
  const mouth = expression === "surprised" ?
    '<circle cx="400" cy="302" r="8" fill="' + fg + '"/>' :
    expression === "shy" ?
    '<path d="M 392 300 Q 400 306 408 300" fill="none" stroke="' + fg + '" stroke-width="6" stroke-linecap="round"/>' : "";
  return eyes + mouth;
}

function pillEyes(theme, expression, gaze) {
  expression = legacyExpression(expression);
  /* Distinct geometry: no outlined rings or pupils. An example of a future
     visual family that keeps all high-level expression IDs unchanged. */
  const fg = color(theme);
  let lh = 115, rh = 115, lw = 112, rw = 112, ly = 210, ry = 210;
  if (expression === "happy" || expression === "shy") lh = rh = 28;
  if (["sleepy","sleeping","blink"].includes(expression)) lh = rh = 18;
  if (expression === "bored") lh = rh = 71;
  if (expression === "drowsy") lh = rh = 40;
  if (expression === "curious") { rh = 76; ry += 11; }
  if (expression === "thinking") rh = 25;
  if (expression === "surprised") { lw = rw = 140; lh = rh = 142; }
  const dx=gaze.x*.21, dy=gaze.y*.17;
  const eye=(cx,y,w,h)=>'<rect x="'+(cx-w/2+dx)+'" y="'+(y-h/2+dy)+
    '" width="'+w+'" height="'+h+'" rx="'+Math.min(w/2,h/2)+'" fill="'+fg+'"/>';
  const mouth=expression==="surprised"?'<circle cx="400" cy="306" r="8" fill="'+fg+'"/>':
    expression==="shy"?'<rect x="390" y="301" width="20" height="6" rx="3" fill="'+fg+'"/>':"";
  return eye(255,ly,lw,lh)+eye(545,ry,rw,rh)+mouth;
}

function eveEyes(theme, expression, gaze) {
  const pose = EVE.expressions.find(e => e.id === expression) ?? EVE.expressions[0];
  // Match native 800x480 dimensions and common attention displacement.
  const w = 232, h = 163, dx = Math.max(-100,Math.min(100,gaze.x)) / 3;
  const dy = Math.max(-100,Math.min(100,gaze.y)) * 480 / 2800;
  function path(shape, i) {
    const points = EVE.shapes[shape];
    const xy = (x,y) => `${(800*(i ? 0.71 : 0.29)+dx+(x-50)*w/100).toFixed(2)} ${(480*.44+dy+(y-50)*h/100).toFixed(2)}`;
    let d = "M " + xy(points[0],points[1]);
    for(let c=0;c<4;c++) {
      const p = points.slice(2+c*6,8+c*6);
      d += " C " + xy(p[0],p[1])+" "+xy(p[2],p[3])+" "+xy(p[4],p[5]);
    }
    return `<path d="${d} Z"/>`;
  }
  const eyes = pose.eyes.map(path).join("");
  // Inline masks/patterns have unique IDs because every settings tile is SVG.
  const key = `eve-${++eveInstance}`;
  return `<defs>
    <filter id="${key}-glow" x="-20%" y="-25%" width="140%" height="150%"><feGaussianBlur stdDeviation="7"/></filter>
    <pattern id="${key}-scan" width="1" height="5" patternUnits="userSpaceOnUse"><rect width="1" height="2" fill="#E3FAFF" fill-opacity=".65"/></pattern>
  </defs><g fill="${theme.eye}" opacity=".65" filter="url(#${key}-glow)">${eyes}</g>
  <g fill="${theme.eye}">${eyes}</g><g fill="url(#${key}-scan)">${eyes}</g>`;
}
let eveInstance = 0;

const renderers = new Map([
  ["outline", outlineEyes],
  ["pill", pillEyes],
  ["eve", eveEyes]
]);
export function registerFaceRenderer(id, renderFn) {
  if (!/^[a-z][a-z0-9_-]*$/.test(id) || typeof renderFn !== "function" || renderers.has(id))
    throw new Error("Renderer already exists or invalid ID");
  renderers.set(id,renderFn);
}
export function renderFace(theme, expression="idle", gaze={x:0,y:0}) {
  const render=renderers.get(theme?.renderer);
  if (!render) throw new Error("Missing appearance renderer: "+theme?.renderer);
  return render(theme,expression,gaze);
}
export function hasFaceRenderer(id) { return renderers.has(id); }
