{-# LANGUAGE FlexibleInstances     #-}
{-# LANGUAGE MultiParamTypeClasses #-}
{-# LANGUAGE NoImplicitPrelude     #-}

{-# OPTIONS -Wall -fwarn-tabs -fno-warn-type-defaults #-}
{-# OPTIONS_GHC -Wno-name-shadowing #-}

module MyData (

  -- * Trie
    Trie(..)
  , isLeaf
  , isRoot
  , makeTrie
  , makeRootTrie
  , insert
  , lookup
  , toString
  , printTrie
  , repr
  , clean
  , loadFile

  -- * Parser
  , WebPage(..)
  , Link
  , Links
  , loadPage
  , isValid
) where

import MyData.Parser (Link, Links, WebPage (..), isValid, loadPage)
import MyData.Trie

