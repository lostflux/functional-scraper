# README

- This directory implements a web scraper in [Haskell](https://www.haskell.org/),
a purely functional programming language.

- The source files are in [src](./src).
- Sample output and some important input files are in [data](./data).

To run the scraper, you need the Haskell toolchain &mdash;
the [ghc](https://www.haskell.org/ghc/) compiler and the [cabal](https://www.haskell.org/cabal/) package manager, both of which can be efficiently installed and managed using [ghcup](https://www.haskell.org/ghcup/).

After installation,

```bash

# 1. This will install project dependencies

cabal install --dependencies-only

# 2. This will run the project.

cabal run

# Make sure you run both in the root directory of the project -- i.e. inside "zeta" 
```
