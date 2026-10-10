# Haskell scraper (legacy, 2022–2023)

This is the original implementation of the crawler, written in
[Haskell](https://www.haskell.org/). It produced the 17,159 documents in
[`../data/log`](../data/log) that make up the 2023 slice of the
[ai-tech-articles](https://huggingface.co/datasets/siavava/ai-tech-articles)
dataset. It is kept for reference and reproducibility; new crawls use the
Rust implementation in [`../rust`](../rust).

- Sources: [`app/Main.hs`](app/Main.hs) (crawl loop),
  [`src/MyData/Parser.hs`](src/MyData/Parser.hs) (HTTP + HXT extraction),
  [`src/MyData/Trie.hs`](src/MyData/Trie.hs) (word-frequency trie).
- Configuration is the shared [`../config.yml`](../config.yml); only the
  `targets`, `domains`, `limit` and `wordcount` keys are read.
- Output format: `../data/log/<id>` (title, year, url, blank line, then a
  sorted word-frequency table) and `../data/log/<id>.txt` (raw text, one
  paragraph per line).

## Running

You need `ghc` and `cabal` (easiest via [ghcup](https://www.haskell.org/ghcup/)).
All paths are relative to this directory, so run from here:

```bash
cd haskell
cabal build
cabal run
```

Caveats, kept as they were:

- As of October 2026 it does not build on this Mac: the installed GHC cannot
  link (`ld: file not found: @…/ghc_NNN.rsp`), so even the `unix` package
  fails. That is a toolchain problem (newer Xcode `ld` vs an old GHC), not a
  source one; a fresh `ghcup install ghc` should fix it.

- It expects a word list at `../data/metadata/dictionary` (git-ignored, not
  shipped) and the previously-seen URL list at `../backups/3/metadata/urls`.
- It writes new documents starting at id 0 and will overwrite
  `../data/log/0`, `../data/log/1`, … Point `logR` elsewhere before running
  it against the committed data.
- Single-threaded, no robots.txt, no relative-link resolution, and the year
  heuristic is "largest 4-digit year in the page text".
