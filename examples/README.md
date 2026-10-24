# Examples

Standalone integrations that talk to voice-pipes from the outside.
- For ready-to-run TOML pipeline configurations covering every major feature (VAP turn detection, Smart Turn, speculative execution, adaptive quality, external gRPC `AgentService`, Realtime S2S + endpoint observer, avatar annotators, client tools, and distributed gRPC workers), see [`voice-pipes/examples/features/README.md`](../voice-pipes/examples/features/README.md).
- Rust examples that exercise engine internals live in `voice-pipes/examples/` (`cargo run --example <name>`).

## `reference-agent/` — protocol-complete gRPC agent (Python)

A minimal but complete implementation of the `AgentService` protocol
(`voice-pipes/proto/agent.proto`): streamed turn replies, the smart-filler
flow with out-of-turn Say, cancel-by-words, idle re-engagement, and the tool
backend for S2S sessions. There is no LLM — replies are canned so the
protocol mechanics stay legible; swap `reply_to()` for your model of choice.

```sh
cd reference-agent
pip install -r requirements.txt

# Generate the gRPC stubs (not checked in):
python -m grpc_tools.protoc -I ../../voice-pipes/proto \
    --python_out=. --grpc_python_out=. agent.proto

python reference_agent.py [--port 50061]
```

Then point a worker at it — see the module docstring in
`reference_agent.py` for the `[agent]` / `[[text_processor]]` config blocks.
