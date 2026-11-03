$env:NVCC_PREPEND_FLAGS="--compiler-options /MD"
cargo run --release -p voice-pipes-cli --features=stt-nemotron,cuda -- --config examples/dev/test-live-nemotron.toml live
