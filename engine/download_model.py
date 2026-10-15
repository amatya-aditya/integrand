"""Download the Nougat LaTeX model (~1.3 GB) into a Hugging Face cache folder.

Usage: python download_model.py <cache_dir>
"""
import sys

from huggingface_hub import hf_hub_download

MODEL = "Norm/nougat-latex-base"
FILES = [
    "config.json",
    "generation_config.json",
    "preprocessor_config.json",
    "special_tokens_map.json",
    "tokenizer.json",
    "tokenizer_config.json",
    "model.safetensors",  # last: the big one
]

for name in FILES:
    hf_hub_download(MODEL, name, cache_dir=sys.argv[1])
    print(f"downloaded {name}", flush=True)
