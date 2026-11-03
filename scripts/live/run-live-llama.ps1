$env:NVCC_PREPEND_FLAGS="--compiler-options /MD"
cargo run --release -p voice-pipes-cli --features=cuda -- --config examples/dev/test-live-llama.toml live
