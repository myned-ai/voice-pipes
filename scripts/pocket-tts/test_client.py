import sys
import os
import grpc
import asyncio

compiled_dir = os.path.dirname(__file__)
sys.path.insert(0, compiled_dir)
import tts_pb2
import tts_pb2_grpc

async def test():
    print("Connecting to gRPC server...")
    async with grpc.aio.insecure_channel('127.0.0.1:50051') as channel:
        stub = tts_pb2_grpc.TtsServiceStub(channel)
        
        print("Sending request for 'Hello, world!'...")
        req = tts_pb2.SynthesizeRequest(text="Hello, world!")
        
        try:
            chunks_received = 0
            async for frame in stub.SynthesizeStream(req):
                print(f"Received audio frame! Sample rate: {frame.sample_rate}, PCM bytes: {len(frame.samples_i16)}")
                chunks_received += 1
                if chunks_received >= 3:
                    break
            print("Successfully received audio from Pocket-TTS over gRPC!")
        except Exception as e:
            print(f"gRPC stream failed: {e}")
            sys.exit(1)

if __name__ == '__main__':
    asyncio.run(test())
