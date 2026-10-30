/*
 * Browser renderers are standalone visual adapters sharing the same theme
 * manifest as the native LVGL renderer registry. Other eye aesthetics can
 * register an implementation here without modifying settings or the app.
 */
const color = theme => theme.eye;

function outlineEyes(theme, expression, gaze) {
  const fg = color(theme), dx = gaze.x * .23, dy = gaze.y * .18;
  let left = "open", right = "open";
  let lw = 148, rw = 148, lh = 136, rh = 136, ly = 212, ry = 212;
  if (expression === "happy" || expression === "shy") left = right = "arch";
  if (expression === "sleepy" || expression === "blink") left = right = "slit";
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
  /* Distinct geometry: no outlined rings or pupils. An example of a future
     visual family that keeps all high-level expression IDs unchanged. */
  const fg = color(theme);
  let lh = 115, rh = 115, lw = 112, rw = 112, ly = 210, ry = 210;
  if (expression === "happy" || expression === "shy") lh = rh = 28;
  if (expression === "sleepy" || expression === "blink") lh = rh = 18;
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

const renderers = new Map([
  ["outline", outlineEyes],
  ["pill", pillEyes]
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
