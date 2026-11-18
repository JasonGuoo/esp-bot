// The console's live face: the same EVE renderer and expression catalogue as the
// browser preview and the device, with irregular blinks and a lazy idle glance.
import { renderFace, FACE_EXPRESSIONS } from "../../../preview/face-renderers.js";

const EVE_THEME = { renderer: "eve", eye: "#35D7FF" };
const NO_BLINK = new Set(["sleeping", "sleepy", "drowsy", "blink", "wink"]);

export type LiveFace = { show(expression: string): void; dispose(): void };

export function mountLiveFace(host: HTMLElement): LiveFace {
  const svg = host.querySelector("svg");
  const known = new Set(FACE_EXPRESSIONS.map((entry) => entry.id));
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  let expression = "idle", blinking = false, disposed = false;
  let gaze = { x: 0, y: 0 };
  let blinkTimer = 0, glanceTimer = 0;
  const draw = (): void => {
    if (!svg || disposed) return;
    const shown = blinking && !NO_BLINK.has(expression) && known.has("blink") ? "blink" : expression;
    const look = expression === "thinking" ? { x: -40, y: -45 } : gaze;
    svg.innerHTML = renderFace(EVE_THEME, shown, look);
    host.dataset.expression = expression;
  };
  const blink = (twice: boolean): void => {
    blinking = true; draw();
    window.setTimeout(() => {
      blinking = false; draw();
      if (twice) blinkTimer = window.setTimeout(() => blink(false), 180);
      else scheduleBlink();
    }, 140);
  };
  const scheduleBlink = (): void => {
    window.clearTimeout(blinkTimer);
    if (disposed || reduced) return;
    // People blink irregularly, sometimes twice.
    blinkTimer = window.setTimeout(() => { if (!document.hidden) blink(Math.random() < 1 / 7); else scheduleBlink(); },
      4200 * (0.55 + Math.random() * 1.1));
  };
  const glance = (): void => {
    if (disposed || reduced) return;
    if (expression === "idle" && !document.hidden) {
      gaze = Math.random() < 0.35 ? { x: 0, y: 0 } : { x: Math.round(Math.random() * 80 - 40), y: Math.round(Math.random() * 40 - 20) };
      draw();
    }
    glanceTimer = window.setTimeout(glance, 2800 + Math.random() * 3500);
  };
  draw(); scheduleBlink(); glanceTimer = window.setTimeout(glance, 2500);
  return {
    show(next: string): void {
      const value = known.has(next) ? next : "idle";
      if (value === expression) return;
      expression = value;
      if (value !== "idle") gaze = { x: 0, y: 0 };
      draw();
    },
    dispose(): void { disposed = true; window.clearTimeout(blinkTimer); window.clearTimeout(glanceTimer); },
  };
}
