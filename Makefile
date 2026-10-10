# Convenience targets; everything runs from the repo root.

BIN := rust/target/release/scraper
DATASET := ../ai-tech-articles

.PHONY: build test crawl crawl-daily check stats dataset publish fmt lint dashboard

build:
	cargo build --release --manifest-path rust/Cargo.toml

test:
	cargo test --workspace --manifest-path rust/Cargo.toml

fmt:
	cargo fmt --all --manifest-path rust/Cargo.toml

lint:
	cargo clippy --workspace --all-targets --manifest-path rust/Cargo.toml

# Full crawl with the budgets in config.yml. The log also goes to
# data/state/crawl.log, which `make dashboard` visualises live.
crawl: build
	$(BIN) crawl $(ARGS) 2>&1 | tee data/state/crawl.log

# Incremental crawl: only sitemap/feed entries newer than the previous run,
# bounded so it fits a scheduled job.
crawl-daily: build
	$(BIN) crawl --min-lastmod auto --max-duration 40m --max-pages 4000 $(ARGS) 2>&1 | tee data/state/crawl.log

# Live progress page for the current (or last) crawl: http://localhost:8765/?budget=5h
dashboard:
	python3 scripts/dashboard.py --port 8765

# make check URL=https://...
check: build
	$(BIN) check-url $(URL) --links

stats: build
	$(BIN) stats

# Rebuild the parquet in the dataset repo from legacy files + new shards.
dataset:
	PYTHONPATH=$(DATASET)/src python3 -m ai_tech_articles.build --scraper-dir . --update-readme

# Upload README + parquet to HuggingFace (needs HF_TOKEN).
publish:
	PYTHONPATH=$(DATASET)/src python3 -m ai_tech_articles.publish
