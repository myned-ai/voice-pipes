# voice-pipes-tts-neutts

Neuphonic NeuTTS-Air Text-to-Speech plugin for [Voice Pipes](https://github.com/myned/voice-pipes).

Synthesizes speech locally on CPU using:
- **Language Model Backbone**: NeuTTS-Air Qwen2 SLM loaded via `candle-transformers` (GGUF format).
- **Acoustic Vocoder**: NeuCodec neural audio decoder loaded via ONNX Runtime (`voice-pipes-onnx`).
- **Phonemizer**: `espeak-ng` IPA conversion through `voice_pipes_sdk::phonemize`.
- **Audio Output**: 24,000 Hz downsampled in-memory to Voice Pipes standard 16,000 Hz mono PCM.
