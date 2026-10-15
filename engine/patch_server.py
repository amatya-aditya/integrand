"""Patch latex_ocr_server for transformers 5.x and add the fast CUDA-graph decoder.

Usage: python patch_server.py   (run with the Python that has latex_ocr_server installed)
Safe to run more than once. Keeps the original as server.py.bak.
"""
import pathlib, shutil, sys
import latex_ocr_server

pkg = pathlib.Path(latex_ocr_server.__file__).parent
server = pkg / "server.py"
here = pathlib.Path(__file__).parent

if not (pkg / "server.py.bak").exists():
    shutil.copy(server, pkg / "server.py.bak")
shutil.copy(here / "fast_decode.py", pkg / "fast_decode.py")

s = server.read_text()
if "fast_decoder" in s:
    if "device = str(self.device)" not in s:
        server.write_text(s.replace("ServerConfig(device = self.device,", "ServerConfig(device = str(self.device),"))
        print(f"patched GetConfig in {server}")
    print(f"{server} is already patched")
    sys.exit(0)

edits = [
    # 1. transformers 5 no longer exposes max_length on the decoder config
    ("max_length=self.model.decoder.config.max_length,",
     'max_length=getattr(self.model.decoder.config, "max_length", None) or 800,'),
    # 2. GetConfig sent a torch.device where protobuf expects a string
    ("ServerConfig(device = self.device,", "ServerConfig(device = str(self.device),"),
    # 3. fast decoder hook
    ("        self.latex_processor = None\n",
     "        self.latex_processor = None\n        self.fast_decoder = None\n"),
    ("""        with torch.no_grad():
            outputs = self.model.generate(
                pixel_values.to(self.device),""",
     """        if self.fast_decoder is not None:
            try:
                sequences = self.fast_decoder(pixel_values)
                return self.decode(sequences)
            except Exception as e:
                print(f"fast decoder failed, falling back to generate(): {e!r}", flush=True)
                self.fast_decoder = None
        with torch.no_grad():
            outputs = self.model.generate(
                pixel_values.to(self.device, self.model.dtype),"""),
    ("        sequence = self.tokenizer.batch_decode(outputs.sequences)[0]",
     """        return self.decode(outputs.sequences)

    def decode(self, sequences):
        sequence = self.tokenizer.batch_decode(sequences)[0]"""),
    ("""        self.latex_processor = NougatLaTexProcessor.from_pretrained(model, cache_dir=self.cache_dir)
        print(" done", flush=True)
""",
     """        self.latex_processor = NougatLaTexProcessor.from_pretrained(model, cache_dir=self.cache_dir)
        print(" done", flush=True)

        if self.device.type == "cuda":
            try:
                from .fast_decode import GraphDecoder
                print("Preparing CUDA graph decoder...", end="", flush=True)
                self.model.to(torch.bfloat16)
                max_length = getattr(self.model.decoder.config, "max_length", None) or 800
                self.fast_decoder = GraphDecoder(self.model, self.tokenizer, max_length)
                self.fast_decoder(self.latex_processor(Image.new("RGB", (64, 64), "white"), return_tensors="pt").pixel_values)
                print(" done", flush=True)
            except Exception as e:
                print(f" failed ({e!r}), using generate()", flush=True)
                self.fast_decoder = None
"""),
]
for old, new in edits:
    if old not in s:
        sys.exit(f"patch failed: could not find expected code in {server}:\n{old}")
    s = s.replace(old, new, 1)
server.write_text(s)
print(f"patched {server}")
