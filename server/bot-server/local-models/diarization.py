"""Local pyannote Community-1 adapter; sample-aligned observations, never authority.
Official offline API: https://huggingface.co/pyannote/speaker-diarization-community-1
The model and its license acceptance are installed by the operator. No network fetch.
"""
import base64
import io
import wave


def diarize_local(worker, payload):
    import sys
    api = sys.modules[type(worker).__module__]
    SAMPLE_RATE, ServiceError, fail = api.SAMPLE_RATE, api.ServiceError, api.fail
    import numpy as np
    import torch

    if worker.diarization_model is None or not (worker.diarization_model / "config.yaml").is_file():
        fail("diarization_not_configured", "Configure a complete local Community-1 snapshot.")
    audio, quality = worker._audio(payload)
    if worker.diarization_pipeline is None:
        from pyannote.audio import Pipeline
        worker.diarization_pipeline = Pipeline.from_pretrained(str(worker.diarization_model.resolve()))
        worker.diarization_pipeline.to(torch.device("cpu"))
    with torch.inference_mode():
        result = worker.diarization_pipeline({"waveform": torch.from_numpy(audio).unsqueeze(0), "sample_rate": SAMPLE_RATE})
    # Use the overlapping annotation, not exclusive diarization: exclusivity
    # would erase simultaneous speakers and falsely certify enrollment quality.
    annotation = result.speaker_diarization
    spans = []
    tracks = ((turn, label) for turn, _, label in annotation.itertracks(yield_label=True))
    for turn, label in tracks:
        start = max(0, min(len(audio), round(turn.start * SAMPLE_RATE)))
        end = max(0, min(len(audio), round(turn.end * SAMPLE_RATE)))
        if end > start:
            spans.append((start, end, str(label)))
    spans.sort()
    if len(spans) > 64:
        fail("diarization_segment_limit", "The recording contains more than 64 speech segments.")
    segments = []
    for start, end, label in spans:
        overlap = any(other != label and a < end and b > start for a, b, other in spans)
        item = {"start_sample": start, "end_sample": end, "sample_rate": SAMPLE_RATE,
                "speaker_label": label, "overlap": overlap, "status": "overlap" if overlap else "unknown",
                "single_speaker_verified": not overlap, "match": {"status": "unknown"}}
        # Never embed mixtures or concatenate across interruptions into a
        # synthetic voice. Short clean spans remain unknown for manual labeling.
        if not overlap and end - start >= 3 * SAMPLE_RATE:
            raw = io.BytesIO()
            with wave.open(raw, "wb") as output:
                output.setnchannels(1); output.setsampwidth(2); output.setframerate(SAMPLE_RATE)
                output.writeframes((np.clip(audio[start:end], -1, 1) * 32767).astype("<i2").tobytes())
            try:
                embedding = worker.speaker_embed({"wav_base64": base64.b64encode(raw.getvalue()).decode("ascii")})
                item.update({"embedding": embedding["embedding"], "model_id": embedding["model_id"],
                             "quality_ok": embedding["quality_ok"], "quality": embedding["quality"], "status": "candidate"})
            except ServiceError as error:
                item["status"] = error.code
        elif not overlap:
            item["status"] = "insufficient_speech"
        segments.append(item)
    return {"model_id": "pyannote/speaker-diarization-community-1", "sample_rate": SAMPLE_RATE,
            "segments": segments, "quality": quality, "identity_claim": False, "timing_basis": "captured_pcm_samples"}
