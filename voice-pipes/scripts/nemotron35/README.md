# Nemotron 3.5 export tooling

`make_export.py` produces this project's own ONNX export of
`nvidia/nemotron-3.5-asr-streaming-0.6b` — the directory the
`voice-pipes-stt-nemotron35` plugin serves and the archive the model release
carries. A user never runs it: `scripts/download_nemotron35.py` fetches the
result. It exists so the hosted files can be regenerated from the NVIDIA
checkpoint by anyone, with the tooling pinned here.

## Environment

CPU is enough; the checkpoint (about 2.5 GB) lands in the Hugging Face cache.

```sh
uv venv --python 3.12 .venv-n35
uv pip install --python .venv-n35/Scripts/python.exe -r voice-pipes/scripts/nemotron35/requirements.txt
```

## Run

```sh
.venv-n35/Scripts/python.exe voice-pipes/scripts/nemotron35/make_export.py --work /tmp/n35 --rev r2
```

The fp32 export step runs the tooling's own ONNX-vs-PyTorch parity checks and
a six-chunk comparison against the Hugging Face streaming path. The result is
gated before it is uploaded by the plugin's token-identity tests, pointed at
the candidate directory:

```sh
NEMOTRON35_MODEL_DIR=/tmp/n35/nemotron-3.5-onnx-320ms cargo test -p voice-pipes-stt-nemotron35 --release
```

Then the archive goes up as a release asset under a data-only tag
(`models-nemotron35-onnx-<rev>`; the tag must not contain a
`digit.digit.digit` run, or cargo-dist's release workflow fires on it) and
`download_nemotron35.py` gets the new tag, archive hash and per-file hashes.

## What is vendored

`vendor/export_onnx.py` and `vendor/quantize.py` are from
[codavidgarcia/nemotron-3.5-asr-streaming-onnx](https://github.com/codavidgarcia/nemotron-3.5-asr-streaming-onnx),
Apache-2.0 (`vendor/LICENSE`), unchanged; the commit is recorded in
`vendor/UPSTREAM`. `LICENSE-OpenMDW-1.1` is NVIDIA's license for the weights
and is copied into every export next to the NOTICE the script writes.
