# 本地语音进程与回退

责任域：Mac。正常 TTS 与首句 TTS 独立；仅完整合成/完整回退才能交付。
Stop 撤销旧 generation，并杀死 Python 与 ffmpeg 子进程组。迟到的旧结果不能
成为新请求的回答。模型不把 MLX 内部音频块当作已交付的设备声音。

需保留的反例顺序：普通通道开始 → 首句到达 → Stop → 新首句 → 旧进程迟到。
若复用含未读响应的旧进程，最后一步可把旧音频标成新请求。实现回归通过真实
JSONL 子进程及其后代验证取消，检查新请求收到自身结果，确认首句不等待普通通道。

运行 `./tools/check-tla-twins.sh macos/LocalSpeech`。音效/完整文本回退用例另在
`test_qwen_robot.py`；统一入口 `./tools/check-functional-regression.sh`。
本模型没有模拟语音质量、GPU 时间或扬声器；这些仍需真实模型与 S31 验收。
