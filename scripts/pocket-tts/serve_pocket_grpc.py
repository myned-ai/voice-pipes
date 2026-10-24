import os
import sys
import grpc
import asyncio
import torch
import numpy as np

# Dynamically compile the protobuf file so we don't have to check in generated files
import grpc_tools.protoc
_repo_root = os.path.join(os.path.dirname(__file__), "..", "..")
proto_dir = os.path.join(_repo_root, "voice-pipes", "proto")
proto_path = os.path.join(proto_dir, "tts.proto")
compiled_dir = os.path.dirname(__file__)

# Compile proto
grpc_tools.protoc.main([
    'grpc_tools.protoc',
    f'-I{proto_dir}',
    f'--python_out={compiled_dir}',
    f'--grpc_python_out={compiled_dir}',
    proto_path,
    os.path.join(proto_dir, "params.proto")
])

# Ensure the compiled files can be imported
sys.path.insert(0, compiled_dir)

import tts_pb2
import tts_pb2_grpc
import params_pb2
import params_pb2_grpc
import pocket_tts
import numpy as np

class TtsService(tts_pb2_grpc.TtsServiceServicer):
    def __init__(self, model):
        self.model = model
        self.voice_states = {}
        self.default_voice = "alba"

    def get_voice_state(self, voice):
        if voice not in self.voice_states:
            self.voice_states[voice] = self.model.get_state_for_audio_prompt(voice)
        return self.voice_states[voice]

    async def GetInfo(self, request, context):
        schema = params_pb2.ParamDescriptor(
            key="voice",
            label="Voice",
            value_type="enum",
            mutable=True,
            description="The default voice to use when hints are absent.",
            constraints=params_pb2.ParamConstraints(
                options=['cosette', 'marius', 'javert', 'alba', 'jean', 'anna', 'vera', 'fantine', 'charles', 'paul', 'eponine', 'azelma', 'george', 'mary', 'jane', 'michael', 'eve', 'bill_boerst', 'peter_yearsley', 'stuart_bell', 'caro_davy', 'giovanni', 'lola', 'juergen', 'rafael', 'estelle']
            )
        )
        return tts_pb2.TtsInfo(
            sample_rate=24000, 
            provider_name="pocket-grpc",
            metadata={"device": "cpu", "model": "kyutai/pocket-tts"},
            param_schema=[schema]
        )

    async def Synthesize(self, request, context):
        context.abort(grpc.StatusCode.UNIMPLEMENTED, "Use SynthesizeStream")

    async def SynthesizeStream(self, request, context):
        text = request.text
        voice = self.default_voice
        if request.HasField("hints") and request.hints.HasField("voice") and request.hints.voice:
            voice = request.hints.voice
            
        print(f"Synthesizing (gRPC): '{text}' using voice '{voice}'")
        
        # PADDING TRICK: Yield 2400 samples (100ms) of silence IMMEDIATELY!
        # This guarantees the Rust client buffer flushes instantly.
        silence = np.zeros(2400, dtype=np.int16)
        yield tts_pb2.AudioFrame(
            samples_i16=silence.tobytes(),
            sample_rate=24000,
            channels=1
        )
        
        # Generator initialization can block, so we run it in a thread
        def init_and_get_generator():
            v_state = self.get_voice_state(voice)
            return self.model.generate_audio_stream(v_state, text)
            
        gen = await asyncio.to_thread(init_and_get_generator)
        
        def get_next_chunk(generator):
            try:
                return next(generator)
            except StopIteration:
                return None
        
        while True:
            try:
                # Run the blocking PyTorch inference in a background thread to prevent starving the gRPC event loop
                chunk = await asyncio.to_thread(get_next_chunk, gen)
                
                if chunk is None:
                    break
                
                # Pocket-TTS yields torch.Tensors of PCM samples
                if hasattr(chunk, 'dtype'):
                    if chunk.dtype in [torch.float32, torch.float64, torch.float16]:
                        arr = (chunk.cpu().numpy() * 32767.0).clip(-32768, 32767).astype(np.int16)
                    else:
                        arr = chunk.cpu().numpy().astype(np.int16)
                else:
                    arr = np.array(chunk, dtype=np.int16)
                
                frame = tts_pb2.AudioFrame(
                    samples_i16=arr.tobytes(),
                    sample_rate=24000,
                    channels=1
                )
                yield frame
                
            except Exception as e:
                print(f"Error during synthesis: {e}")
                await context.abort(grpc.StatusCode.INTERNAL, str(e))

class ParamServiceImpl(params_pb2_grpc.ParamServiceServicer):
    def __init__(self, tts_service):
        self.tts_service = tts_service

    async def UpdateParams(self, request, context):
        if request.key == "voice":
            self.tts_service.default_voice = request.value.str_val
            return params_pb2.UpdateParamsResponse(applied=request.value)
        
        context.abort(grpc.StatusCode.INVALID_ARGUMENT, f"Unknown parameter {request.key}")

async def serve():
    print("Loading TTS model into memory (this prevents cold starts!)...")
    model = pocket_tts.TTSModel.load_model()
    
    server = grpc.aio.server()
    tts_service = TtsService(model)
    tts_pb2_grpc.add_TtsServiceServicer_to_server(tts_service, server)
    params_pb2_grpc.add_ParamServiceServicer_to_server(ParamServiceImpl(tts_service), server)
    
    listen_addr = '[::]:50051'
    server.add_insecure_port(listen_addr)
    print(f"Starting Pocket-TTS gRPC server on {listen_addr}...")
    await server.start()
    await server.wait_for_termination()

if __name__ == '__main__':
    asyncio.run(serve())
