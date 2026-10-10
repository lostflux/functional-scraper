#!/usr/bin/env bash
# One incremental crawl. Used by the GitHub Actions workflow (crawl only;
# the dataset repo's workflow builds and publishes) and by the local launchd
# job (which also builds and publishes, since it has the token).
#
# Env:
#   MAX_DURATION     wall-clock budget for the crawl (default 40m)
#   MAX_PAGES        fetch budget (default 4000)
#   COMMIT=1         commit new shards + state in this repo
#   BUILD_DATASET=1  also rebuild the parquet in ../ai-tech-articles
#   PUBLISH=1        …and publish it: commit + push to HuggingFace and GitHub (token from its .env/HF_TOKEN)
#   DATASET_DIR      path to the ai-tech-articles checkout (default ../ai-tech-articles)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATASET="${DATASET_DIR:-$ROOT/../ai-tech-articles}"
cd "$ROOT"

cargo build --release --manifest-path rust/Cargo.toml
rust/target/release/scraper crawl \
  --min-lastmod auto \
  --max-duration "${MAX_DURATION:-40m}" \
  --max-pages "${MAX_PAGES:-4000}" 2>&1 | tee data/state/crawl.log

if [ "${COMMIT:-0}" = "1" ]; then
  git add data/articles data/state/seen.bin data/state/next_id data/state/last_run.json
  if ! git diff --cached --quiet; then
    git alt commit -m "(update): add crawl shards from $(date -u +%F)" 2>/dev/null \
      || git -c user.name=evilalt -c user.email=amittaijoel@gmail.com \
           commit --no-gpg-sign -m "(update): add crawl shards from $(date -u +%F)"
  fi
fi

if [ "${BUILD_DATASET:-0}" = "1" ] && [ -d "$DATASET" ]; then
  uv run --project "$DATASET" ai-tech-articles-build --scraper-dir "$ROOT" --update-readme
  if [ "${PUBLISH:-0}" = "1" ]; then
    # publish commits the parquet + card (as evilalt) and pushes to both remotes
    uv run --project "$DATASET" ai-tech-articles-publish --as-bot \
      --message "(update): refresh dataset from crawl $(date -u +%F)"
  fi
fi
