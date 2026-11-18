// The browser preview's shared renderer (preview/face-renderers.js), reused as is.
declare module "*/face-renderers.js" {
  export function renderFace(theme: { renderer: string; eye: string }, expression?: string, gaze?: { x: number; y: number }): string;
  export const FACE_EXPRESSIONS: { id: string }[];
}
