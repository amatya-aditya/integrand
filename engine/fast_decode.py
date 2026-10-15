"""Fast greedy decoding for the Nougat (VisionEncoderDecoder + mBART) model on CUDA.

transformers' generate() spends most of its time on per-token Python/launch
overhead. This runs the mBART decoder step with a static KV cache and replays it
as a CUDA graph, feeding each predicted token back in on the GPU. Output matches
generate(..., bad_words_ids=[[unk]]) greedy decoding.
"""
import threading
import torch
import torch.nn.functional as F


class GraphDecoder:
    def __init__(self, model, tokenizer, max_len, chunk=16):
        self.model, self.max_len, self.chunk = model, max_len, chunk
        self.d = model.decoder.model.decoder
        self.lm_head = model.decoder.lm_head
        cfg = model.decoder.config
        self.H, self.D = cfg.decoder_attention_heads, cfg.d_model // cfg.decoder_attention_heads
        self.eos, self.unk = tokenizer.eos_token_id, tokenizer.unk_token_id
        self.bos = tokenizer(tokenizer.bos_token, add_special_tokens=False).input_ids[0]
        self.device, self.dtype = model.device, model.dtype
        self.graph = None
        self.enc_len = None
        self.lock = threading.Lock()

    def _alloc(self, enc_len):
        L, dev, dt = len(self.d.layers), self.device, self.dtype
        self.enc_len = enc_len
        self.k = torch.zeros(L, 1, self.H, self.max_len, self.D, device=dev, dtype=dt)
        self.v = torch.zeros_like(self.k)
        self.ck = torch.zeros(L, 1, self.H, enc_len, self.D, device=dev, dtype=dt)
        self.cv = torch.zeros_like(self.ck)
        self.tok = torch.zeros(1, 1, dtype=torch.long, device=dev)
        self.pos = torch.zeros(1, dtype=torch.long, device=dev)
        self.ar = torch.arange(self.max_len, device=dev)
        self.hist = torch.full((self.max_len + self.chunk,), self.eos, dtype=torch.long, device=dev)
        self.graph = None

    def _heads(self, x):
        return x.view(1, -1, self.H, self.D).transpose(1, 2)

    def _reset(self):
        self.tok.fill_(self.bos)
        self.pos.zero_()

    def _step(self):
        d = self.d
        p = self.pos.clamp(max=self.max_len - 1)
        h = d.embed_tokens(self.tok) + d.embed_positions.weight[p + d.embed_positions.offset].unsqueeze(0)
        h = d.layernorm_embedding(h)
        mask = (self.ar <= p).view(1, 1, 1, self.max_len)
        for i, layer in enumerate(d.layers):
            a = layer.self_attn
            x = layer.self_attn_layer_norm(h)
            self.k[i].index_copy_(2, p, self._heads(a.k_proj(x)))
            self.v[i].index_copy_(2, p, self._heads(a.v_proj(x)))
            o = F.scaled_dot_product_attention(self._heads(a.q_proj(x)), self.k[i], self.v[i], attn_mask=mask)
            h = h + a.out_proj(o.transpose(1, 2).reshape(1, 1, -1))
            a = layer.encoder_attn
            x = layer.encoder_attn_layer_norm(h)
            o = F.scaled_dot_product_attention(self._heads(a.q_proj(x)), self.ck[i], self.cv[i])
            h = h + a.out_proj(o.transpose(1, 2).reshape(1, 1, -1))
            x = layer.final_layer_norm(h)
            h = h + layer.fc2(layer.activation_fn(layer.fc1(x)))
        logits = self.lm_head(d.layer_norm(h))[:, -1]
        logits[:, self.unk] = float('-inf')
        nxt = logits.argmax(-1)
        self.hist.index_copy_(0, self.pos, nxt)
        self.tok.copy_(nxt.view(1, 1))
        self.pos.add_(1)

    def _capture(self):
        st = torch.cuda.Stream()
        st.wait_stream(torch.cuda.current_stream())
        with torch.cuda.stream(st):
            for _ in range(2):
                self._reset()
                for _ in range(self.chunk):
                    self._step()
        torch.cuda.current_stream().wait_stream(st)
        self._reset()
        self.graph = torch.cuda.CUDAGraph()
        with torch.cuda.graph(self.graph):
            for _ in range(self.chunk):
                self._step()

    @torch.no_grad()
    def __call__(self, pixel_values):
        with self.lock:
            enc = self.model.encoder(pixel_values=pixel_values.to(self.device, self.dtype)).last_hidden_state
            if getattr(self.model, 'enc_to_dec_proj', None) is not None:
                enc = self.model.enc_to_dec_proj(enc)
            if enc.shape[1] != self.enc_len:
                self._alloc(enc.shape[1])
            for i, layer in enumerate(self.d.layers):
                self.ck[i].copy_(self._heads(layer.encoder_attn.k_proj(enc)))
                self.cv[i].copy_(self._heads(layer.encoder_attn.v_proj(enc)))
            if self.graph is None:
                self._capture()
            self._reset()
            self.hist.fill_(self.eos)
            n = 0
            while n < self.max_len - 1:
                self.graph.replay()
                new = self.hist[n:n + self.chunk].tolist()
                if self.eos in new:
                    n += new.index(self.eos) + 1
                    break
                n += self.chunk
            n = min(n, self.max_len - 1)
            return torch.tensor([[self.bos] + self.hist[:n].tolist()])
