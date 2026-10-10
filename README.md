# functional-scraper

A focused web crawler that collects AI / technology articles. It is the
collection side of the
[ai-tech-articles](https://huggingface.co/datasets/siavava/ai-tech-articles)
dataset (source repo: [`../ai-tech-articles`](https://github.com/lostflux/ai-tech-articles)).

Two implementations live here:

| dir | status | what |
| --- | --- | --- |
| [`rust/`](rust) | current | Parallel, polite, breadth-first crawler. Produces `data/articles/*.jsonl.zst`. |
| [`haskell/`](haskell) | legacy (2022–2023) | The original single-threaded crawler. Produced `data/log/`. Kept for reproducibility. |

Both read the shared [`config.yml`](config.yml). A `domains` entry covers
exactly its host (with or without `www.`) and path prefix; subdomains such as
`shop.theverge.com` are out of scope unless listed.

## Layout

```
config.yml            keywords, sources, crawl limits (shared)
data/
  log/                legacy output: <id> (title/year/url/word counts) + <id>.txt (text); ids 0..17158
  articles/           new output: one zstd-compressed JSONL shard per crawl run
  state/
    seen.bin          visited URLs as 16-byte (xxh3 hash, day, flags) records
    next_id           next record id (continues the legacy numbering)
    last_run.json     summary of the previous run
    frontier.jsonl    leftover queue for `--resume` (not committed)
backups/              earlier legacy crawls (unchanged)
rust/                 Cargo workspace (see below)
haskell/              legacy implementation
scripts/daily.sh      one incremental crawl + dataset rebuild
scripts/*.plist       launchd template for running daily.sh locally
.github/workflows/    the same job on GitHub Actions
```

## Running

```bash
make build                       # cargo build --release in rust/
make crawl                       # full crawl with the budgets in config.yml
make crawl-daily                 # incremental: feeds/sitemaps since the last run, 40 min cap
make check URL=https://…         # fetch one page and print what the extractor sees
make stats                       # what is stored, by year and domain, plus the last run
make dataset                     # rebuild ../ai-tech-articles/data/*.parquet from log/ + articles/
make dashboard                   # live progress page at http://localhost:8765/?budget=5h
```

`make crawl` tees the log to `data/state/crawl.log`; `make dashboard` tails
that file into a page with the stored-article count, throughput, queue size,
per-minute and per-domain and per-year breakdowns, and a feed of the latest
hits (`scripts/dashboard.py` + `scripts/dashboard.html`, no dependencies).
Pass `?budget=5h` matching the run's `--max-duration` to get a countdown.

`scraper crawl` accepts `--max-pages`, `--max-hits`, `--max-duration 30m`,
`--min-lastmod 2024-01-01|auto`, `--scope open|domains`, `--seed URL`
(repeatable) and `--resume`. Ctrl-C once finishes in-flight fetches and
saves the queue; twice abandons them. Set `RUST_LOG=debug` to see every
fetch decision.

## How the Rust crawler works

`rust/` is a workspace of small crates, one per concern:

| crate | concern |
| --- | --- |
| `scraper-core` | `config.yml` schema; URL normalisation, scoping rules, hashing |
| `scraper-net` | HTTP client, `robots.txt` cache, size caps, charset decoding |
| `scraper-extract` | HTML → title / text / date / language / links; sitemap and feed parsing; keyword rule |
| `scraper-store` | hashed seen-set, JSONL shards, id counter, run summaries |
| `scraper-crawl` | frontier (per-host queues, politeness, probation) and the async engine |
| `scraper` (bin) | the CLI |

Design points, mapped to the original wish list:

- **Visited lookup by hash.** URLs are normalised (scheme/host lowercased,
  `www.` kept but `?query`, `#fragment`, default ports and trailing slashes
  dropped) and stored as 64-bit xxh3 hashes, 16 bytes per record including a
  day stamp and a hit flag. Hits are never refetched; non-hits (home,
  section and listing pages) are refetched after `revisit_after_days` so new
  links keep surfacing. A canonical-URL hash is recorded too, so AMP and
  tracking variants dedupe.
- **Page processing.** html5ever parse, text from block elements inside
  `<article>`/`<main>`/`<body>` with nav/header/footer/aside/script pruned.
  Dates come from `article:published_time`, JSON-LD `datePublished`,
  `<time datetime>`, the URL path, bylines, and only then the legacy
  "largest year in the text" fallback, and the record says which one was
  used (`date_source`). Pages are only stored when they carry an article
  signal (`og:type`, article-typed JSON-LD, dated metadata or a dated URL)
  and most of their text sits in paragraph-length blocks
  (`min_prose_ratio`), so home, section and tag pages are crawled for links
  but not kept. Keywords may be phrases (`machine learning`).
- **Breadth-first, across sites, weighted by yield.** The frontier keeps a
  queue per host and serves hosts round-robin, so one prolific site cannot
  monopolise the run (the legacy data is 64% VentureBeat). Within a host,
  links found on relevant pages are served before the rest. Each host also
  carries a moving average of its recent hit ratio; after a warm-up it gets
  turns in proportion to that ratio relative to the best host (never below
  `min_host_weight`), so fetch budget flows to the sites producing articles
  while low-yield hosts are still sampled. The rule is work-conserving.
- **Relevance has two tiers.** The broad `targets` vocabulary (legacy
  `wordcount` rule) plus `strong_targets`: terms only an AI story uses. A
  page must mention strong terms at least `min_strong_mentions` times and
  carry one in its title or lede, or have a body dense with them. This is
  what keeps general tech articles whose site chrome says "AI" out; it
  removed 44% of what the broad rule alone accepted in testing. The dataset
  build applies the same rule to new rows (`--no-relevance-filter` to
  disable), so stored records can be re-filtered when the rule changes.
- **Parallelism.** Tokio with `crawl.concurrency` fetches in flight,
  `per_host_concurrency` per host, a per-host minimum gap (raised by
  `Crawl-delay`), HTML parsing on the blocking pool, and a single owner for
  all mutable state so there are no locks on the hot path.
- **Discovery.** Seeds can be pages, RSS/Atom feeds or sitemaps (indexes
  included); `robots.txt` sitemaps are picked up automatically and
  `min_lastmod` skips entries older than the previous run, which is what
  makes a daily run cheap. Seeds are fetched again every
  `reseed_every_secs` (also on `--resume`), so long runs keep finding fresh
  articles, and links on pages older than `min_lastmod` are not followed,
  so archives do not pull the crawl into the past.
- **Beyond the fixed domain list.** `crawl.scope: open` follows links to any
  host, but only from pages that scored as relevant (topic locality), with
  a deny-list for social/search/shopping sites, a per-host page cap, and
  probation: a host outside `domains` that has not produced
  `host_min_hit_ratio` hits after `host_probation_pages` fetches is dropped.
  Cross-site feeds (Google News / Hacker News searches in `seeds`) give an
  open crawl its entry points.

## Sources

`config.yml` lists about fifty sites: the original tech and AI outlets, the
labs (OpenAI, DeepMind, Anthropic, Cohere, Meta AI, Google, NVIDIA,
Microsoft Research, AWS ML, Hugging Face, Mistral) and the mainstream
outlets that serve article text to a crawler (NY Post, BBC, CNBC, CNN, NBC,
Fox, TIME, Semafor, ZDNet, Ars Technica, Engadget, The Register, Quanta, The
Conversation, Scientific American, 404 Media, Rest of World). The New York
Times, AP, Reuters, WSJ, Washington Post, Politico, Bloomberg, Axios, The
Atlantic and The Information answer 401/403 or time out for non-browser
clients and are therefore absent. `scraper check-url <url>` shows what the
crawler sees on any page, which is how new sources were vetted.

## Output format

One JSON object per line in `data/articles/<run-id>.jsonl.zst`:

```json
{"id": 17159, "url": "https://…", "domain": "technologyreview.com",
 "title": "…", "date": "2024-03-04", "date_source": "jsonld", "year": 2024,
 "text": "paragraph\nparagraph\n…", "keywords": ["ai", "learning", …],
 "fetched_at": "2026-10-09T18:12:33Z"}
```

Compared with the legacy numbered files this is one file per run instead of
two per document, compressed about 5×, directly `zstdcat | jq`-able, and
append-only so shards never conflict in git. `ai_tech_articles.build` (in
`../ai-tech-articles`) merges legacy files and shards (deduplicating by URL) into the parquet that
HuggingFace serves. The legacy `data/log/` is untouched.

## Keeping the dataset growing

Two workflows, one per repo, so each repo holds only its own secret:

1. **Crawl** ([`.github/workflows/daily-crawl.yml`](.github/workflows/daily-crawl.yml),
   this repo, 06:30 UTC daily). Runs `scripts/daily.sh` (incremental crawl:
   `--min-lastmod auto`, 40 minutes, 4,000 fetches), commits the new shard
   and `data/state/` as `evilalt`, then fires a `repository_dispatch`
   event (`crawl-finished`) at `lostflux/ai-tech-articles`. Author and
   pusher are the `evilalt` utility account when `ACTOR_TOKEN` (a
   fine-grained PAT from that account with "Contents: read and write" on
   both repos) is set, which also fires the trigger; without it the author
   is still evilalt, the push comes from github-actions, the trigger step
   is skipped, and the dataset workflow runs on its weekly schedule or by
   hand.
2. **Build and publish** (`.github/workflows/build-dataset.yml` in the
   dataset repo). Checks out both repos side by side, runs
   `ai-tech-articles-build`, then `ai-tech-articles-publish`, which commits
   the parquet (LFS pointer) and the dataset card and pushes the same commit
   to HuggingFace and GitHub: one history, two remotes; the parquet bytes
   live only on HuggingFace.

Free on public repos. Caveats: publishers that block or throttle datacenter
IPs (Bloomberg, Axios, The Atlantic, VentureBeat's rate limiter) contribute
little from a runner, and GitHub disables cron workflows after 60 days
without commits, which the data commits prevent.

The same script runs locally, where the crawl sees a residential IP:
`scripts/com.siavava.scraper.plist` is a launchd template that runs
`daily.sh` with `COMMIT=1 BUILD_DATASET=1 PUBLISH=1` at 06:30 (the publish
step reads the dataset repo's git-ignored `.env`). A free-tier VM (Oracle
always-free ARM, GCP e2-micro) with a cron line is the third option, with
the datacenter-IP caveat but no job time limit.

The crawler is idempotent across runs: `seen.bin` carries over, ids continue
from `state/next_id`, and each run writes its own shard, so running it from
several places into one repo only requires pulling before and pushing after.

## Legacy data

`data/log/` holds the 17,159 documents the Haskell crawler collected, all
committed. The `backups/` directories are earlier passes of the same
crawler. Nothing in these directories is modified by the Rust crawler; the
only link is that new ids start where the legacy ones stop.
