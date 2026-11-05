# MAW Lab: take a language model apart

`maw lab` is a language model small enough to understand completely. It is a real Transformer (the same
kind of model as GPT, Claude or Llama) written from scratch in Rust, with **27,000 parameters** instead of
billions, so a student can train it on **their own text in a minute or two on an ordinary laptop** and then
look at what happened inside. Nothing is borrowed from a framework: the tokenizer, the embeddings, the
attention, back-propagation, the AdamW optimizer, the learning-rate schedule, weight decay and sampling
are all in [`crates/maw-lab`](../crates/maw-lab), and every number on screen is read from the running model.

It does **not** need Ollama, a GPU, a download, an account or the network.

## Start in one line

```bash
maw lab                         # trains on a built-in text and opens the live 3D view
maw lab train notes.txt         # ...on your own UTF-8 text file
maw lab train notes.txt --plain # no full-screen view: progress lines, saves lab-model.json
```

Then, with the model you saved (`w` in the view, or `--plain`):

```bash
maw lab sample lab-model.json --prompt "The model " --temperature 0.8 --top-k 8
maw lab tokens lab-model.json "any text"       # how the tokenizer cuts it
maw lab info   lab-model.json                  # sizes, where the parameters are, how it was trained
```

Useful options of `train`: `--steps`, `--dim` (embedding width), `--layers`, `--heads`, `--ctx` (context
window), `--lr`, `--schedule constant|cosine|exponential`, `--warmup`, `--wd` (weight decay),
`--clip`, `--tokenizer char|bpe:N`, `--seed` (same seed = the same run, bit for bit), `--demo en|vi`,
`--paused` (start paused; Space begins). `maw lab train --help` lists them all.

## What the screen shows

| Panel | What it is | Concepts |
|---|---|---|
| **Embeddings** (left, 3D) | Every token is a point; the model's `dim` numbers per token are squeezed into 3 directions by PCA. The points really move while it trains. Rotate with the arrow keys, zoom with `+`/`-`. | tokens, embeddings, vector spaces, PCA |
| **Loss** | Training loss (cyan) and loss on text the model never trained on (yellow). | loss, cross-entropy, perplexity, over-fitting |
| **Learning rate** | The schedule you chose, with a dot for "now". | warm-up, learning-rate decay, optimisers |
| **Attention** | For a sentence you can change (`p`), how much each token looks at the earlier ones, per layer and head (`l`, `h`). The upper triangle is empty: the future cannot be seen. | self-attention, causal mask, heads |
| **Lesson** (bottom left, `Tab`) | Ten short lessons that quote the numbers on screen right now. | tokenizer, embedding, attention, loss, learning rate, weight decay, over-fitting, sampling, context window / KV cache / RoPE decay, parameters |
| **What the model writes** | Text it generates from the current weights; `s` writes again. | sampling |

Turning the 3D view (use whichever your terminal passes on): the arrow keys, or `,` `.` (left/right) and
`[` `]` (up/down) if the arrows do not reach the program; dragging with the left mouse button; or a command
such as `/rotate 45 20` or `/view top`. The title of the panel always shows the angle in use. The view turns
by itself only after four quiet seconds and stops the moment you touch anything (`/spin off` or `a` stops
it for good). `+`/`-` or the wheel zoom.

Other keys: `Space` pause/resume, `Tab`/`Shift+Tab` or `1`-`9`/`0` lessons, `l`/`h` layer/head, `p` type a
sentence to study, `s` sample, `w` save, `q` quit.

**Commands.** Type `/` and a menu of every command appears (Tab completes, Enter runs, Esc cancels):
`/rotate <yaw> [pitch]` (degrees), `/view front|side|top|iso`, `/zoom <0.4-6>`, `/spin on|off`, `/pause`,
`/resume`, `/lesson <1-10>`, `/layer <n>`, `/head <n>`, `/probe <sentence>`, `/sample`, `/save [file]`,
`/help`, `/quit`. The answer, or the reason a command was refused, shows in the bottom line.

It needs a terminal of at least 60x22 and uses Braille characters for the 3D drawing. The mouse is optional:
under Windows `cmd` the terminal may keep it for selecting text, and everything has a key or a command.

## Exercises

1. **Watch it learn.** Run `maw lab`. At step 0 the sample is noise; by step 300 it spells real fragments.
   Rotate the embeddings: do the vowels end up near each other?
2. **See over-fitting.** The built-in text is tiny (about 2,000 tokens). Training loss falls to about 0.4
   while the yellow held-out loss rises: the model is memorising. Fix it by giving it more text, or
   `--dim 16`, or `--wd 0.1`, and compare the curves.
3. **Compare schedules.** Train three times with `--seed 1` and `--schedule constant`, `cosine`,
   `exponential`. Same seed, so the only difference is the learning-rate curve.
4. **Tokenizers.** `--tokenizer bpe:100` then `maw lab tokens lab-model.json "..."`: common letter pairs
   became single tokens, so the same text takes fewer tokens (and therefore fewer context-window slots).
5. **Temperature.** `maw lab sample ... --temperature 0` (always the favourite) against `1.5` (adventurous),
   and `--top-k 3`.
6. **Size.** Compare `--layers 1 --dim 16` with `--layers 4 --dim 64`: parameters (`maw lab info`),
   training speed and final held-out loss.
7. **Your own data.** Any UTF-8 text: a chapter of a book, song lyrics, code. A few thousand characters
   is enough to see structure; a few hundred thousand starts to look like the real thing.

## How it relates to real models (and where it does not)

Same: token and position embeddings, residual connections, RMS normalisation, multi-head causal attention,
a feed-forward block with GELU, weight-tied output layer, next-token cross-entropy, AdamW with warm-up and
decay, gradient clipping, temperature/top-k sampling.

Different, on purpose: it is tiny; it works on characters (or a small BPE vocabulary) rather than a
100,000-token one; it learns position vectors instead of using **RoPE** (lesson 9 shows RoPE's decaying
rotation speeds from the real formula); it trains on a CPU in `f64`; it has no mixture-of-experts, no
grouped-query attention, no quantisation and no reinforcement-learning stage. What it writes after a minute
of training is not good English, and it is not supposed to be: the point is that it is **yours to inspect**.

## Is the code right?

The back-propagation is written by hand, so it is checked against numerical differentiation
(`cargo test -p maw-lab`): 152 parameters spread over every tensor agree with finite differences to a
relative error of about 3e-8. The tests also check that attention never looks forward, that a run is
reproducible from its seed, that weight decay spares the gains and biases, that the schedules reach their
floors, and that a saved model loads back identically.

---

## Tiếng Việt (tóm tắt cho sinh viên)

`maw lab` là một mô hình ngôn ngữ **đủ nhỏ để hiểu hết**: một Transformer thật (cùng loại với GPT) được viết
từ đầu, chỉ có khoảng 27.000 tham số. Bạn tự huấn luyện nó trên **văn bản của chính bạn** trong một hai phút
ngay trên laptop, rồi nhìn vào bên trong: các điểm embedding trong không gian 3D di chuyển khi học, đường
cong loss và learning rate, bản đồ attention của một câu bạn gõ, và những gì mô hình viết ra.

```bash
maw lab                      # chạy ngay trên văn bản mẫu có sẵn (tiếng Anh)
maw lab --help
maw lab train --demo vi      # văn bản mẫu tiếng Việt
maw lab train van-ban.txt    # văn bản của bạn (UTF-8)
```

Không cần Ollama, không cần GPU, không cần tải gì thêm. Mười bài học trong màn hình (phím `Tab`) giải thích:
tokenizer, embedding, attention, loss và perplexity, learning rate và decay, weight decay, over-fitting
(học vẹt), lấy mẫu và nhiệt độ, cửa sổ ngữ cảnh/KV cache/RoPE, và các tham số nằm ở đâu — luôn dùng chính con
số đang chạy. Mô hình nhỏ nên văn bản nó viết sẽ chưa hay; mục đích là để bạn **tự tay huấn luyện và thấy từng
bước**.
