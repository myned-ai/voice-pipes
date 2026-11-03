#!/usr/bin/env python3
"""
Downloads the Moonshine ASR ONNX models and tokenizer config from Hugging Face
into the local `models/moonshine` directory using the official huggingface_hub library.

The decoder is the merged export, which decodes with a key/value cache. Pass
`--with-plain-decoder` to also fetch `decoder_model.onnx` (no cache), which the
provider falls back to when the merged one is absent and which the
`cached_decoder_matches_full_prefix_decoder` test compares against.
"""

import argparse
import os
import shutil
import sys
from huggingface_hub import hf_hub_download

# Configuration
REPO_ID = "onnx-community/moonshine-base-ONNX"
TARGET_DIR = os.path.join("models", "moonshine")

FILES_TO_DOWNLOAD = [
    {
        "repo_path": "onnx/encoder_model.onnx",
        "local_name": "encoder_model.onnx"
    },
    {
        "repo_path": "onnx/decoder_model_merged.onnx",
        "local_name": "decoder_model_merged.onnx"
    },
    {
        "repo_path": "tokenizer.json",
        "local_name": "tokenizer.json"
    }
]

PLAIN_DECODER = {
    "repo_path": "onnx/decoder_model.onnx",
    "local_name": "decoder_model.onnx"
}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--with-plain-decoder", action="store_true",
                        help="also download decoder_model.onnx (no key/value cache)")
    args = parser.parse_args()
    files = FILES_TO_DOWNLOAD + ([PLAIN_DECODER] if args.with_plain_decoder else [])

    # Make sure target dir exists
    os.makedirs(TARGET_DIR, exist_ok=True)
    print(f"Target directory: {TARGET_DIR}")

    for file_info in files:
        local_path = os.path.join(TARGET_DIR, file_info["local_name"])
        print(f"\nDownloading {file_info['repo_path']} from Hugging Face...")

        try:
            # Download file using official huggingface_hub
            cached_path = hf_hub_download(
                repo_id=REPO_ID,
                filename=file_info["repo_path"]
            )
            # Copy to local models folder
            shutil.copy(cached_path, local_path)
            print(f"Successfully copied to: {local_path}")
        except Exception as e:
            print(f"Error downloading {file_info['local_name']}: {e}")
            sys.exit(1)

    print("\nAll Moonshine base ONNX files successfully downloaded!")

if __name__ == "__main__":
    main()
