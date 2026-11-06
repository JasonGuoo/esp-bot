# Upstream reuse and provenance

This directory is integration code. Model inference, feature extraction and
system speech are called through the maintained upstream interfaces below.
No model weights are committed to this repository, no upstream training
pipeline is vendored, and no claim of real-device validation is made.

## Apple MLX Whisper

- Source: https://github.com/ml-explore/mlx-examples/tree/796f5b53cab69a3d48a44233ce21aae889e94a08/whisper
- License: MIT; copyright Apple Inc.
- API reused: `mlx_whisper.transcribe` with in-memory NumPy audio and a local
  `path_or_hf_repo`; the upstream `ModelHolder` cache is cleared by `unload`.
- Original Whisper: https://github.com/openai/whisper ; MIT code/model license.
- Converted model: https://huggingface.co/mlx-community/whisper-small-mlx/tree/45f3915923c7a79a5a5b5a7d909d39aeb0e5630e
  The conversion card does not itself declare a separate license. It identifies
  the original `small` Whisper model. Do not describe it as an OpenAI-published
  MLX conversion or infer new commercial guarantees from its absence of a field.

## SpeechBrain ECAPA

- Source: https://github.com/speechbrain/speechbrain/tree/v1.1.2
- Library license: Apache-2.0.
- Model: https://huggingface.co/speechbrain/spkrec-ecapa-voxceleb/tree/0f99f2d0ebe89ac095bcc5903c4dd8f72b367286
- Model card license: Apache-2.0.
- API reused: `EncoderClassifier.from_hparams`, `encode_batch`; local
  `pretrained_path`, `LocalStrategy.NO_LINK`, and offline `FetchConfig`.
- The VoxCeleb training domain is not a validation claim for Chinese children,
  far-field home microphones, overlap, spoof detection or authorization.

## OpenCV Zoo

- Source: https://github.com/opencv/opencv_zoo/tree/48b10fa505aeb51fbd127fc60a5c660040e823b7/models
- YuNet directory license: MIT, copyright Shiqi Yu and attributed contributors.
- SFace directory license: Apache-2.0; its example source credits the Shenzhen
  Institute of Artificial Intelligence and Robotics for Society (2021) and
  respective third-party authors.
- API pattern reused from `yunet.py` and `sface.py`: `FaceDetectorYN.create`,
  `setInputSize`, `detect`, `FaceRecognizerSF.create`, `alignCrop`, `feature`.
  The wrappers are not copied wholesale. The local quality policy, JSONL
  framing, enrollment handoff and cancellation are project code.
- The provisioner downloads the corresponding license files alongside ONNX
  weights. Preserve those upstream licenses when redistributing model bundles.

## macOS speech synthesis

- Uses Apple's installed `/usr/bin/say`; does not redistribute macOS voices.
- API reference: local `man say`, and Apple's speech synthesis sample at
  https://developer.apple.com/documentation/avfaudio/creating-a-custom-speech-synthesizer
- Speech output rights and system voice availability follow the applicable
  installed OS/voice terms. There is no voice-cloning implementation here.

The model manifest separates code, weights and platform-provided resources.
Package dependencies retain their own licenses. The repository does not add a
claim that an upstream benchmark or license statement certifies this product.
