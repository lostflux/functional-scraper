{-# LANGUAGE BangPatterns        #-}
{-# LANGUAGE ImportQualifiedPost #-}

module Main where

import Control.Arrow ((&&&))
import Control.Monad (when, (<$!>))
import Data.List     (foldl', isPrefixOf)
import Data.Set      (Set, (\\))
import Data.Set      qualified as Set
import GHC.IO.Unsafe (unsafePerformIO)
import MyData.Parser (Link, Links, WebPage (..), isValid, config)
import MyData.Parser qualified as Parser
import MyData.Trie   (Trie (..), (<|>))
import MyData.Trie   qualified as Trie
import System.Exit   (exitSuccess)
import Text.Printf   (printf)
import Data.Foldable ( Foldable(foldl'), for_ ) 


-- | Set to true to log debug info.
debug :: Bool
debug = False

limit :: Int
limit = Parser.limit config

main :: IO ()
main = do
  putStrLn "\nStarting..."
  crawl
  putStrLn "\nDone.\n"

-- | The number of matches we need to approve a page.
matchCount :: Int
matchCount = Parser.wordcount config

seedURLs :: [Link]
seedURLs = Parser.domains config

{-# NOINLINE prevPages #-}
prevPages :: [Link]
prevPages = unsafePerformIO $ do
  urls <- lines <$!> readFile "backups/3/metadata/urls"
  return $! filter isAllowed $ reverse urls


isAllowed :: String -> Bool
isAllowed url = any (`isPrefixOf` url) seedURLs


crawl :: IO ()
crawl = do
  let docID = 0
  let allWords = EmptyTrie
  let allLinks = []
  -- !prevPages <- filter (not . null) <$!> lines <$!> readFile "data/metadata/urls"
  let !urls = prevPages
  let !seenURLs = Set.fromList urls

  -- launch the crawl.
  iter urls seenURLs docID allWords

-- | The main loop.
iter :: [Link] -> Links -> Int -> Trie -> IO ()
iter [] seenURLs docID allWords = do
  -- let dir = "data/metadata"
  -- writeFile (printf "%s/urls" dir) $! unlines $! Set.toList seenURLs
  -- writeFile (printf "%s/all" dir) $! show allWords
  printf "Seen %d unique URLs.\n" (Set.size seenURLs)
  printf "THE END"
  exitSuccess

iter queue@(url:rest) seenURLs docID allWords = do
  -- let !(url, rest) = (head &&& tail) queue
  -- if Set.member url seenURLs
  --   then do
  --     printf "%sIgnDupl:  %s%s\n" yellow url reset
  --     iter rest seenURLs docID allWords
  -- else do
  printf "%sFetch:      %s%s\n" green url reset
  !page <- Parser.loadPage url
  if isValid page then do
    let !words = allWords <|> text page
    let !asList = filter isAllowed $ Set.toList (links page \\ seenURLs)
    let !s = foldl' (flip Set.insert) seenURLs asList
    let !q = rest ++ asList
    let status = hasKeyWords page
    when debug $ printf "\n\t\tqueue length : %d\n\t\tseen urls: %d\n\n" (length q) (Set.size seenURLs)
    if fst status then do
      printf "%sHit %5d : %s%s\n" blue docID url reset
      case docID of
        0 -> writeFile "data/metadata/urls" $ url ++ "\n"
        _ -> appendFile "data/metadata/urls" $ url ++ "\n"
      logR docID url page
      iter q s (docID + 1) words
    else iter q s docID words
  else do
    printf "%sIgnBad:     %s%s\n" red url reset
    -- let !filtered = filter (not . isPrefixOf url) rest
    iter rest seenURLs docID allWords

advance :: [Link] -> Links -> Int -> Trie -> [String] -> IO ()
advance q s docID words allLinks
  | length q > limit = iter (drop (length q - (limit `div` 2)) q) s docID words
  | otherwise = iter q s docID words

-- | Does the page have any of the specified set of keywords?
hasKeyWords :: WebPage -> (Bool, Int)
hasKeyWords page =
  check (Parser.targets config) $! text page
    where
      check :: [String] -> Trie -> (Bool, Int)
      check [] _        = (True, 0)
      check _ EmptyTrie = (False, 0)
      check words trie = (count >= matchCount, count)
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
