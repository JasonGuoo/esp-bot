/** Explicit, transient local capture. No network, storage, background wake or
 * identity inference occurs here. Official Web Audio/Media Capture patterns:
 * https://developer.mozilla.org/en-US/docs/Web/API/AudioWorkletNode
 * https://developer.mozilla.org/en-US/docs/Web/API/OfflineAudioContext
 * https://developer.mozilla.org/en-US/docs/Web/API/MediaDevices/getUserMedia
 */

export type CapturedWav = { base64: string; seconds: number; bytes: number };
type CaptureCallbacks = { progress: (seconds: number) => void; limit: () => void };

const OUTPUT_RATE = 16_000;
const MAX_SECONDS = 30;
const MAX_WAV_BYTES = 1_000_000;
const MAX_IMAGE_BYTES = 4 * 1024 * 1024;

export class PcmRecorder {
  private stream: MediaStream | null = null;
  private context: AudioContext | null = null;
  private source: MediaStreamAudioSourceNode | null = null;
  private node: AudioWorkletNode | null = null;
  private gain: GainNode | null = null;
  private chunks: Float32Array[] = [];
  private frames = 0;
  private generation = 0;
  private timer: number | null = null;
  private deadline: number | null = null;
  private flushDone: (() => void) | null = null;
  private active = false;
  private finishing = false;
  private reachedLimit = false;
  private workletLimited = false;

  get recording(): boolean { return this.active && !this.finishing; }

  async start(callbacks: CaptureCallbacks): Promise<void> {
    if (this.active) throw new Error("录音已开始，请先结束这一段。");
    if (!navigator.mediaDevices?.getUserMedia || !window.AudioContext) {
      throw new Error("当前窗口不支持麦克风采集。可以选择 16 kHz 单声道 PCM16 WAV 文件。");
    }
    const generation = ++this.generation;
    this.active = true;
    this.finishing = false;
    this.reachedLimit = false;
    this.workletLimited = false;
    this.frames = 0;
    this.chunks = [];
    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true }, video: false,
      });
      if (generation !== this.generation) {
        stream.getTracks().forEach((track) => track.stop());
        throw new Error("capture_cancelled");
      }
      this.stream = stream;
      const context = new AudioContext();
      this.context = context;
      if (!context.audioWorklet || context.sampleRate < 8_000 || context.sampleRate > 192_000) {
        throw new Error("当前音频设备无法使用 AudioWorklet，请选择 WAV 文件。");
      }
      // Force a separate asset: the app's script-src 'self' intentionally does
      // not allow Vite's default small-asset data: URL inlining.
      await context.audioWorklet.addModule(new URL("./capture-worklet.js?no-inline", import.meta.url).href);
      if (generation !== this.generation) throw new Error("capture_cancelled");
      const node = new AudioWorkletNode(context, "xiaodai-pcm-capture", {
        numberOfInputs: 1, numberOfOutputs: 1, outputChannelCount: [1],
        channelCount: 1, channelCountMode: "explicit", processorOptions: { maxSeconds: MAX_SECONDS },
      });
      this.node = node;
      const limit = () => {
        if (this.reachedLimit || generation !== this.generation || !this.active) return;
        this.reachedLimit = true;
        callbacks.limit();
      };
      node.port.onmessage = (event: MessageEvent<unknown>) => {
        if (generation !== this.generation) return;
        const data = event.data as { type?: string; buffer?: unknown };
        if (data.type === "pcm" && data.buffer instanceof ArrayBuffer) {
          const chunk = new Float32Array(data.buffer);
          if (chunk.length > 4096 || this.frames + chunk.length > Math.ceil(context.sampleRate * MAX_SECONDS)) {
            limit();
            return;
          }
          this.frames += chunk.length;
          this.chunks.push(chunk);
        } else if (data.type === "finished") {
          this.flushDone?.();
        } else if (data.type === "limit") {
          this.workletLimited = true;
          limit();
        }
      };
      this.source = context.createMediaStreamSource(stream);
      this.gain = context.createGain();
      this.gain.gain.value = 0;
      this.source.connect(node);
      node.connect(this.gain);
      this.gain.connect(context.destination);
      await context.resume();
      if (generation !== this.generation) throw new Error("capture_cancelled");
      callbacks.progress(0);
      this.timer = window.setInterval(() => callbacks.progress(this.frames / context.sampleRate), 200);
      this.deadline = window.setTimeout(limit, MAX_SECONDS * 1000);
    } catch (error) {
      if (generation === this.generation) this.cancel();
      throw error;
    }
  }

  async finish(): Promise<CapturedWav> {
    if (!this.active || this.finishing || !this.context || !this.node) {
      throw new Error("当前没有可结束的录音。");
    }
    this.finishing = true;
    const generation = this.generation;
    const sampleRate = this.context.sampleRate;
    // Preserve the final partial worklet chunk before closing the device.
    if (!this.workletLimited) {
      await new Promise<void>((resolve) => {
        const timeout = window.setTimeout(resolve, 400);
        this.flushDone = () => { window.clearTimeout(timeout); resolve(); };
        this.node?.port.postMessage({ type: "finish" });
      });
    }
    if (generation !== this.generation) throw new Error("capture_cancelled");
    const chunks = this.chunks;
    const frames = this.frames;
    this.chunks = [];
    this.frames = 0;
    this.releaseDevices();
    this.active = false;
    if (generation !== this.generation) throw new Error("capture_cancelled");
    if (frames < sampleRate * 0.25) throw new Error("录音至少需要 0.25 秒，请再说一次。");
    const samples = new Float32Array(frames);
    let offset = 0;
    for (const chunk of chunks) { samples.set(chunk, offset); offset += chunk.length; }
    const outputFrames = Math.min(OUTPUT_RATE * MAX_SECONDS, Math.floor(frames * OUTPUT_RATE / sampleRate));
    const offline = new OfflineAudioContext(1, outputFrames, OUTPUT_RATE);
    const input = offline.createBuffer(1, frames, sampleRate);
    input.getChannelData(0).set(samples);
    const source = offline.createBufferSource();
    source.buffer = input;
    source.connect(offline.destination);
    source.start();
    // Use the browser's resampler rather than hand-written signal processing.
    const rendered = await offline.startRendering();
    if (generation !== this.generation) throw new Error("capture_cancelled");
    const wav = encodeWav(rendered.getChannelData(0));
    return { base64: bytesToBase64(wav), seconds: outputFrames / OUTPUT_RATE, bytes: wav.byteLength };
  }

  cancel(): void {
    this.generation += 1;
    this.flushDone?.();
    this.chunks = [];
    this.frames = 0;
    this.active = false;
    this.finishing = false;
    this.releaseDevices();
  }

  private releaseDevices(): void {
    if (this.timer !== null) window.clearInterval(this.timer);
    if (this.deadline !== null) window.clearTimeout(this.deadline);
    this.timer = this.deadline = null;
    this.flushDone = null;
    this.stream?.getTracks().forEach((track) => track.stop());
    this.stream = null;
    this.source?.disconnect();
    this.source = null;
    if (this.node) { this.node.port.onmessage = null; this.node.disconnect(); }
    this.node = null;
    this.gain?.disconnect();
    this.gain = null;
    if (this.context && this.context.state !== "closed") void this.context.close().catch(() => {});
    this.context = null;
  }
}

export class LocalCamera {
  private stream: MediaStream | null = null;
  private video: HTMLVideoElement | null = null;
  private generation = 0;

  async open(video: HTMLVideoElement): Promise<void> {
    this.close();
    const generation = this.generation;
    if (!navigator.mediaDevices?.getUserMedia) throw new Error("当前窗口无法打开摄像头，可选择 JPEG / PNG 照片。");
    const stream = await navigator.mediaDevices.getUserMedia({
      video: { width: { ideal: 960 }, height: { ideal: 720 }, facingMode: "user" }, audio: false,
    });
    if (generation !== this.generation) {
      stream.getTracks().forEach((track) => track.stop());
      throw new Error("capture_cancelled");
    }
    this.stream = stream;
    this.video = video;
    video.srcObject = stream;
    video.muted = true;
    video.playsInline = true;
    try {
      await video.play();
      if (generation !== this.generation) throw new Error("capture_cancelled");
    } catch (error) { if (generation === this.generation) this.close(); throw error; }
  }

  snapshot(): string {
    const video = this.video;
    if (!video || !this.stream || video.readyState < 2 || !video.videoWidth || !video.videoHeight) {
      throw new Error("摄像头画面还没准备好，请稍等。");
    }
    const scale = Math.min(1, 960 / Math.max(video.videoWidth, video.videoHeight));
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.round(video.videoWidth * scale));
    canvas.height = Math.max(1, Math.round(video.videoHeight * scale));
    const context = canvas.getContext("2d");
    if (!context) throw new Error("无法创建相机快照。");
    context.drawImage(video, 0, 0, canvas.width, canvas.height);
    const data = canvas.toDataURL("image/jpeg", 0.9).split(",")[1];
    if (!data || data.length > MAX_IMAGE_BYTES * 4 / 3) throw new Error("照片大小超过限制。");
    return data;
  }

  close(): void {
    this.generation += 1;
    this.stream?.getTracks().forEach((track) => track.stop());
    this.stream = null;
    if (this.video) { this.video.pause(); this.video.srcObject = null; }
    this.video = null;
  }
}

export async function readWavFile(file: File): Promise<CapturedWav> {
  if (file.size > MAX_WAV_BYTES || file.size < 44) throw new Error("WAV 文件应为 30 秒以内的 16 kHz 单声道 PCM16 音频。");
  const bytes = new Uint8Array(await file.arrayBuffer());
  const seconds = validateWav(bytes);
  return { base64: bytesToBase64(bytes), seconds, bytes: bytes.length };
}

export async function readImageFile(file: File): Promise<string> {
  if (file.size > MAX_IMAGE_BYTES || file.size < 8) throw new Error("请选择 4 MB 以内的 JPEG / PNG 照片。");
  const bytes = new Uint8Array(await file.arrayBuffer());
  const jpeg = bytes[0] === 0xff && bytes[1] === 0xd8;
  const png = [137, 80, 78, 71, 13, 10, 26, 10].every((value, index) => bytes[index] === value);
  if (!jpeg && !png) throw new Error("人脸样本只接受 JPEG 或 PNG。");
  return bytesToBase64(bytes);
}

export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000));
  }
  return btoa(binary);
}

export function base64ToBuffer(base64: string, maxBytes: number): ArrayBuffer {
  if (base64.length > Math.ceil(maxBytes * 4 / 3) + 4) throw new Error("媒体数据超过窗口的大小限制。");
  const binary = atob(base64);
  if (binary.length > maxBytes) throw new Error("媒体数据超过窗口的大小限制。");
  const buffer = new ArrayBuffer(binary.length);
  const bytes = new Uint8Array(buffer);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  return buffer;
}

function validateWav(bytes: Uint8Array): number {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const tag = (offset: number) => String.fromCharCode(...bytes.subarray(offset, offset + 4));
  if (tag(0) !== "RIFF" || tag(8) !== "WAVE") throw new Error("文件不是 RIFF/WAVE 音频。");
  const end = view.getUint32(4, true) + 8;
  if (end > bytes.length || end < 44) throw new Error("WAV 文件长度不完整。");
  let validFormat = false;
  let audioBytes = 0;
  for (let offset = 12; offset + 8 <= end;) {
    const kind = tag(offset);
    const size = view.getUint32(offset + 4, true);
    const start = offset + 8;
    if (start + size > end) throw new Error("WAV 文件区块不完整。");
    if (kind === "fmt ") {
      if (size < 16) throw new Error("WAV 格式头不完整。");
      validFormat = view.getUint16(start, true) === 1 && view.getUint16(start + 2, true) === 1
        && view.getUint32(start + 4, true) === OUTPUT_RATE && view.getUint16(start + 12, true) === 2
        && view.getUint16(start + 14, true) === 16;
    } else if (kind === "data") {
      if (audioBytes) throw new Error("只支持单一 data 区块的 WAV 文件。");
      audioBytes = size;
    }
    offset = start + size + (size % 2);
  }
  const seconds = audioBytes / 2 / OUTPUT_RATE;
  if (!validFormat || audioBytes % 2 || seconds < 0.25 || seconds > MAX_SECONDS) {
    throw new Error("请选择 0.25–30 秒、16 kHz、单声道、PCM16 WAV 文件。");
  }
  return seconds;
}

function encodeWav(samples: Float32Array): Uint8Array {
  const bytes = new Uint8Array(44 + samples.length * 2);
  const view = new DataView(bytes.buffer);
  const writeTag = (offset: number, tag: string) => {
    for (let index = 0; index < tag.length; index += 1) bytes[offset + index] = tag.charCodeAt(index);
  };
  writeTag(0, "RIFF"); view.setUint32(4, bytes.length - 8, true); writeTag(8, "WAVE");
  writeTag(12, "fmt "); view.setUint32(16, 16, true); view.setUint16(20, 1, true);
  view.setUint16(22, 1, true); view.setUint32(24, OUTPUT_RATE, true);
  view.setUint32(28, OUTPUT_RATE * 2, true); view.setUint16(32, 2, true); view.setUint16(34, 16, true);
  writeTag(36, "data"); view.setUint32(40, samples.length * 2, true);
  for (let index = 0; index < samples.length; index += 1) {
    const sample = Number.isFinite(samples[index]) ? Math.max(-1, Math.min(1, samples[index])) : 0;
    view.setInt16(44 + index * 2, Math.round(sample * (sample < 0 ? 32768 : 32767)), true);
  }
  return bytes;
}
