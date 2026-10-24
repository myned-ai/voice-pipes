import argparse
import struct
import numpy as np
import torch
import uvicorn
from fastapi import FastAPI, Form
from fastapi.responses import StreamingResponse
from pocket_tts import TTSModel

def main():
    parser = argparse.ArgumentParser(description="Streaming Pocket-TTS Server")
    parser.add_argument("--host", type=str, default="127.0.0.1", help="Host to bind to")
    parser.add_argument("--port", type=int, default=8000, help="Port to bind to")
    args = parser.parse_args()

    app = FastAPI()

    print("Loading TTS model into memory (this prevents cold starts!)...")
    model = TTSModel.load_model()
    print("Model loaded successfully!")

    def make_wav_header(sample_rate=24000, num_channels=1, bytes_per_sample=2):
        """
        Creates a WAV header. We use 0xFFFFFFFF for the sizes because we stream 
        the audio and don't know the final length upfront.
        The Rust client only looks for the 'data' chunk marker and ignores the length.
        """
        header = b"RIFF" + struct.pack("<I", 0xFFFFFFFF) + b"WAVE"
        header += b"fmt " + struct.pack("<I", 16) + struct.pack("<H", 1) + struct.pack("<H", num_channels)
        header += struct.pack("<I", sample_rate) + struct.pack("<I", sample_rate * num_channels * bytes_per_sample)
        header += struct.pack("<H", num_channels * bytes_per_sample) + struct.pack("<H", bytes_per_sample * 8)
        header += b"data" + struct.pack("<I", 0xFFFFFFFF)
        return header

    @app.post("/tts")
    async def tts_endpoint(text: str = Form(...), voice_url: str = Form("alba")):
        # Pre-process the voice state
        voice_state = model.get_state_for_audio_prompt(voice_url)
        
        async def audio_generator():
            # 1. Send the WAV header immediately
            yield make_wav_header()
            
            # 2. PADDING TRICK: Yield 2400 samples (100ms) of silence!
            # The Rust client's STREAM_CHUNK_SAMPLES is 2400. 
            # Pocket-TTS only yields 1920 samples per chunk. 
            # Because 1920 < 2400, your Rust pipeline sits and waits for the SECOND 
            # PyTorch chunk to arrive before it plays anything! 
            # By yielding 2400 samples of silence immediately, we force Rust to flush 
            # instantly, dropping your TTFB to near zero. The silence is imperceptible.
            silence = np.zeros(2400, dtype=np.int16)
            yield silence.tobytes()
            
            # 3. Start the streaming generation using asyncio to prevent thread blocking
            import asyncio
            gen = model.generate_audio_stream(voice_state, text)
            
            while True:
                try:
                    # Run the PyTorch generation safely in a background thread
                    chunk = await asyncio.to_thread(next, gen)
                except StopIteration:
                    break
                except Exception as e:
                    print(f"Error during generation: {e}")
                    break
                    
                # Pocket-TTS yields torch.Tensors of PCM samples
                if hasattr(chunk, 'dtype'):
                    if chunk.dtype != torch.int16:
                        if chunk.dtype in [torch.float32, torch.float64, torch.float16]:
                            arr = (chunk.cpu().numpy() * 32767.0).astype(np.int16)
                        else:
                            arr = chunk.cpu().numpy().astype(np.int16)
                    else:
                        arr = chunk.cpu().numpy()
                    
                    yield arr.tobytes()
                else:
                    yield chunk

        # Return a chunked streaming response
        return StreamingResponse(audio_generator(), media_type="audio/wav")

    uvicorn.run(app, host=args.host, port=args.port)

if __name__ == "__main__":
    main()
