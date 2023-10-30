{-# LANGUAGE BangPatterns        #-}
{-# LANGUAGE ImportQualifiedPost #-}

module Main where

import Control.Arrow ((&&&))
import Control.Monad (when, (<$!>))
import Data.List     (foldl', isPrefixOf)
import Data.Set      (Set, (\\))
import Data.Set      qualified as Set
import GHC.IO.Unsafe (unsafePerformIO)
import MyData.Parser (Link, Links, WebPage (..), isValid, targets)
import MyData.Parser qualified as Parser
import MyData.Trie   (Trie (..), (<|>))
import MyData.Trie   qualified as Trie
import System.Exit   (exitSuccess)
import Text.Printf   (printf)


-- | Set to true to log debug info.
debug :: Bool
debug = False

limit :: Int
limit = 5000

main :: IO ()
main = do
  putStrLn "\nStarting..."
  crawl
  putStrLn "\nDone.\n"

-- | The number of matches we need to approve a page.
matchCount :: Int
matchCount = 5

seedURLs :: [Link]
seedURLs = [
  -- "https://github.com/siavava"
    "https://www.technologyreview.com"
  , "https://www.deepmind.com"
  , "https://singularityhub.com"
  , "https://www.wired.com"
  , "https://www.vox.com"
  , "https://www.theverge.com"
  , "https://www.theguardian.com"
  , "https://www.theatlantic.com"
  , "https://www.washingtonpost.com"
  , "https://www.techcrunch.com"
    -- "https://singularityhub.com/2022/04/20/gm-just-patented-a-self-driving-car-that-teaches-people-to-drive"
  ]


crawl :: IO ()
crawl = do
  let docID = 0
  let allWords = EmptyTrie
  let allLinks = []
  !prevPages <- filter (not . null) <$!> lines <$!> readFile "data/metadata/urls"
  let !urls = prevPages ++ seedURLs
  let !seenURLs = Set.fromList urls
  iter urls seenURLs docID allWords

iter :: [Link] -> Links -> Int -> Trie -> IO ()
iter queue seenURLs docID allWords = do
  when (null queue || docID >= limit) $! do
    let dir = "data/metadata"
    writeFile (printf "%s/all" dir) $! show allWords
    printf "Seen %d unique URLs.\n" (Set.size seenURLs + length queue)
    printf "THE END"
    exitSuccess

  let !(url, rest) = (head &&& tail) queue
  -- if Set.member url seenURLs then do
  --   printf "%sIgnDupl:  %s%s\n" yellow url reset
  --   iter rest seenURLs docID allWords
  -- else do
  printf "%sFetch:      %s%s\n" green url reset
  !page <- Parser.loadPage url
  if isValid page then do
    let !words = allWords <|> text page
    let !asList = Set.toList (links page \\ seenURLs)
    let !s = foldl' (flip Set.insert) seenURLs asList
    let !q = rest ++ asList
    when debug $ printf "\n\t\tqueue length : %d\n\t\tseen urls: %d\n\n" (length q) (Set.size seenURLs)
    if hasKeyWords page then do
      printf "%sHit  %5d: %s%s\n" blue docID url reset
      logR docID url page
      iter q s (docID + 1) words
    else iter q s docID words
  else do
    printf "%sIgnBad:     %s%s\n" red url reset
    let !filtered = filter (not . isPrefixOf url) rest
    iter filtered seenURLs docID allWords

advance :: [Link] -> Links -> Int -> Trie -> [String] -> IO ()
advance q s docID words allLinks
  | length q > limit = iter (drop (length q - (limit `div` 2)) q) s docID words
  | otherwise = iter q s docID words

-- | Does the page have any of the specified set of keywords?
hasKeyWords :: WebPage -> Bool
hasKeyWords page =
  check targets $! text page
    where
      check :: [String] -> Trie -> Bool
      check [] _        = True
      check _ EmptyTrie = False
      check words trie = count >= matchCount
        where
          !count = foldl' (\acc x -> if Trie.lookup x trie then acc + 1 else acc) 0 words

logR :: Int -> Link -> WebPage -> IO ()
logR docID url page = do
  let !file = printf "data/log/%d" docID
  let !rawFile = printf "data/log/%d.txt" docID
  writeFile file $! printf "%s\n%s\n%s\n\n%s\n" ttl yr url $! show (text page)
  writeFile rawFile $! raw page
    where
      ttl = title page
      yr  = year page

--- colors
type Color = String
blue, cyan, green, red, yellow, reset :: Color
blue      = "\x1b[94m"
cyan      = "\x1b[96m"
green     = "\x1b[92m"
red       = "\x1b[31m"
yellow    = "\x1b[93m"
reset     = "\x1b[0m"
