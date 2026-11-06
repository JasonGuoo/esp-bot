// Standard Web Audio AudioWorkletProcessor pattern. Microphone is opened only
// by the explicit parent-console action in capture.ts, never by this module.
// Reference: https://developer.mozilla.org/en-US/docs/Web/API/AudioWorkletProcessor
class XiaodaiPcmCapture extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const requested = Number(options.processorOptions?.maxSeconds ?? 30);
    this.limit = Math.floor(sampleRate * Math.min(30, Math.max(1, requested)));
    this.captured = 0;
    this.buffer = new Float32Array(4096);
    this.used = 0;
    this.running = true;
    this.port.onmessage = (event) => {
      if (event.data?.type !== "finish") return;
      this.running = false;
      this.flush();
      this.port.postMessage({ type: "finished" });
    };
  }

  flush() {
    if (this.used === 0) return;
    const chunk = this.buffer.slice(0, this.used);
    this.used = 0;
    this.port.postMessage({ type: "pcm", buffer: chunk.buffer }, [chunk.buffer]);
  }

  process(inputs) {
    if (!this.running) return false;
    const channels = inputs[0];
    if (!channels?.length || !channels[0]?.length) return true;
    const frames = Math.min(channels[0].length, this.limit - this.captured);
    for (let frame = 0; frame < frames; frame += 1) {
      let sample = 0;
      for (const channel of channels) sample += channel[frame] ?? 0;
      this.buffer[this.used++] = sample / channels.length;
      if (this.used === this.buffer.length) this.flush();
    }
    this.captured += frames;
    if (this.captured >= this.limit) {
      this.running = false;
      this.flush();
      this.port.postMessage({ type: "limit" });
      return false;
    }
    // Output channels are intentionally untouched (silence): no mic feedback.
    return true;
  }
}

registerProcessor("xiaodai-pcm-capture", XiaodaiPcmCapture);
