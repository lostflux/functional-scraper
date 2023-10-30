
# Metadata

This directory contains metadata about my corpus.

In particular:

- [all](all) contains a list of all the words, accumulated across all the files.
- [all.bk](./all.bk) is a back-up of [all](./all).
- [dictionary](./dictionary) is a list of words I used as "official" English words
  in my scraping. I generated the dictionary after running into a few issues with invalid words
  and even non-English words showing up in my results, which I didn't want at the moment.
- [index.py](./index.py) is a script that generates the index files (except the Excel, I had to do that manually) and the totals.
- [index.csv](./index.csv), [index.tsv](./index.tsv), and [index.xlsx](./index.xlsx)
  are identical CSV, TSV,, and Excel files.
  each row is a document id, year,  url for the corresponding document.
- [urls](./urls) is a convenient list of all the URLS. I plan to use this later to expand my corpus.
- [total](./total) contains the sum of all the words-counts across all the files.

I built a web crawler for data collection.
I've currently been learning [Haskell](https://www.haskell.org/),
so I took it as a nice design challenge to do that in the language.
I also found that more efficient than Python, since Haskell is a compiled language.
Here's a GitHub [link](https://github.com/siavava/tau/tree/main/zeta)
to the specific directory containing the scraper
(the repository contains a bunch of other irrelevant stuff).

&copy; Amittai, 2022.
