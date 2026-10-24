import time
import pocket_tts
import torch
import numpy as np

def run_benchmark():
    print("Loading TTS model...")
    model = pocket_tts.TTSModel.load_model()
    voice = "alba"
    text = "Hello! This is a test of the raw synthesis performance for Pocket TTS without any gRPC or HTTP overhead."
    
    print(f"Synthesizing text: '{text}'")
    print("Warming up model...")
    # Warm up (run inference once and discard)
    voice_state = model.get_state_for_audio_prompt(voice)
    gen = model.generate_audio_stream(voice_state, text)
    for _ in gen: pass

    print("Running benchmark...")
    
    start_time = time.perf_counter()
    
    # 1. Get voice state
    voice_state = model.get_state_for_audio_prompt(voice)
    
    # 2. Generate stream
    gen = model.generate_audio_stream(voice_state, text)
    
    first_chunk_time = None
    chunks = 0
    total_samples = 0
    
    try:
        while True:
            chunk = next(gen)
            if first_chunk_time is None:
                first_chunk_time = time.perf_counter() - start_time
            chunks += 1
            if hasattr(chunk, 'shape'):
                # Handle torch.Tensor
                total_samples += chunk.shape[-1]
            else:
                total_samples += len(chunk) // 2
    except StopIteration:
        pass
        
    total_time = time.perf_counter() - start_time
    audio_duration = total_samples / 24000.0
    rtf = total_time / audio_duration if audio_duration > 0 else 0
    
    print("\n--- Benchmark Results ---")
    print(f"Time to First Chunk (TTFB): {first_chunk_time * 1000:.2f} ms")
    print(f"Total Synthesis Time:       {total_time * 1000:.2f} ms")
    print(f"Generated Audio Duration:   {audio_duration * 1000:.2f} ms")
    print(f"Real-Time Factor (RTF):     {rtf:.2f}x")
    if rtf > 1.0:
        print("  --> WARNING: RTF > 1.0 means synthesis is slower than real-time (will cause stuttering)")
    else:
        print("  --> OK: Synthesis is faster than real-time playback")
    print(f"Chunks yielded:             {chunks}")
    
if __name__ == "__main__":
    run_benchmark()
