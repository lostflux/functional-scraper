{-# LANGUAGE ImportQualifiedPost #-}
{-# LANGUAGE BangPatterns #-}

module MyData.Parser (
    loadPage
  , WebPage(..)
  , Link
  , Links
  , isValid
  , targets
) where

import Control.Arrow                  (arr, (<+>))
import Control.Arrow.IOStateListArrow (IOSLA)
import Control.Exception              (try)
import Data.ByteString.Lazy           (ByteString)
import Data.ByteString.Lazy           qualified as ByteString
import Data.ByteString.Lazy.UTF8      qualified as ByteString
import Data.Char                      (isDigit, isSpace)
import Data.List                      (dropWhileEnd, isInfixOf, isPrefixOf,
                                       isSuffixOf, foldl')
import Data.Set                       (Set)
import Data.Set                       qualified as Set
import MyData.Trie                    (Trie (..), clean, insert, loadFile,
                                       makeRootTrie, (>|<))
import MyData.Trie                    qualified as Trie
import Network.HTTP.Conduit           (HttpException, simpleHttp)
import System.IO.Unsafe               (unsafePerformIO)
import Text.Printf                    (printf)
import Text.XML.HXT.Arrow.XmlState    (XIOState)
import Text.XML.HXT.Core              (ArrowTree (deep, multi, (//>)),
                                       ArrowXml (getAttrValue, getText, hasAttr, hasAttrValue, hasName, isText),
                                       XNode (XText), XmlTree, no, readString,
                                       runX, withParseHTML, withWarnings, yes,
                                       (>>>))
import Text.XML.HXT.DOM.XmlNode       (NTree (..))

data WebPage = EmptyPage | WebPage {
    title :: String
  , year  :: String
  , links :: Links
  , text  :: Trie
  , raw   :: String
}

instance Show WebPage where
  show EmptyPage = "EmptyPage"
  show page = printf "WebPage {\ntitle = %s\n\nyear = %s\n\nlinks = %s\n\ntext = \n%s}" ttl yr lnks txt
    where
      ttl = title page
      yr  = year page
      lnks = show $! links page
      txt = show $! text page

isValid :: WebPage -> Bool
isValid EmptyPage = False
isValid _         = True

type Link = String
type Links = Set Link

-- | Keywords to search for.
--
-- Add keywords here.
targets :: [String]
targets = [
      "machine"
    , "machines"
    , "learning"
    , "deep"
    , "artificial"
    , "intelligence"
    , "neural"
    , "network"
    , "thinking"
    , "reinforcement"
    , "ai"
    , "recommender"
    , "system"
    , "systems"
    , "mind"
    , "language"
    , "processing"
    , "computer"
    , "vision"
  ]

domains :: [String]
domains = [
      "technologyreview.com"
    , "deepmind.com"
    , "singularityhub.com"
    , "wired.com"
    , "vox.com"
    , "theverge.com"
    , "theguardian.com"
    , "theatlantic.com"
    , "washingtonpost.com"
    , "techcrunch.com"
  ]


{-# NOINLINE dictionary #-}
dictionary :: Trie
dictionary =
  let !dict = unsafePerformIO $! loadFile "data/metadata/dictionary"
  in foldl' (flip Trie.insert) dict targets

checkDict :: String -> Bool
checkDict word = Trie.lookup word dictionary

loadPage :: Link -> IO WebPage
loadPage url = do
  !raw <- try $! simpleHttp url :: IO (Either HttpException ByteString)
  case raw of
    Left _     -> return EmptyPage
    Right text -> do
      let !html = ByteString.toString text
      let !doc = readString [withParseHTML yes, withWarnings no] html
      (txt, raw) <- getWords doc
      !ttl   <- getTitle doc
      !lnks  <- getLinks url doc
      !yr    <- getYear doc
      -- validWords <- (txt >|<) <$> dictionary
      return $! WebPage ttl yr lnks txt raw

-- | Get the title of the page.
--
-- Returns an empty string if the title is not found.
getTitle :: IOSLA (XIOState ()) XmlTree (NTree XNode) -> IO String
getTitle doc = do
  ttl <- runX $! doc
    //> hasName "title"
    //> deep (isText >>> getText)
  return $! getFirst ttl

-- | Get all the words in a webpage.
--
-- Returns a Trie.
getWords :: IOSLA (XIOState ()) XmlTree (NTree XNode) -> IO (Trie, String)
getWords doc = do
  text <- runX $! doc
    //> ( hasName "p"
      <+> hasName "h1" <+> hasName "h2"
      <+> hasName "h3" <+> hasName "h4"
      <+> hasName "h5" <+> hasName "h6"
      <+> hasName "li" <+> hasName "span"
    ) //> (isText >>> getText)
    >>> arr words
    -- >>> arr (filter checkDict)
  let !trie = foldl' (foldr insert) EmptyTrie $!
        map (filter (\x -> checkDict x && not ("author" `isInfixOf` x || "http" `isInfixOf` x)) . map clean) text
  let !raw = unlines $! fix $! map unwords text
  return (trie, raw)
  -- ! review the change here!! How does it affect functionality?

getYear :: IOSLA (XIOState ()) XmlTree (NTree XNode) -> IO String
getYear doc = do
  yrs <- runX $! doc
    //> hasName "p"
    //> getText
    >>> arr checkYear
  let !lastYear = dropYear "" $! filter (not . null) yrs
  -- print yrs
  -- print lastYear
  return lastYear

years :: [String]
years = map show [1000..2022]

checkYear :: String -> String
checkYear str = iter $! words str
  where
  iter [] = ""
  iter (w:ws)
    | w `elem` years = w
    | otherwise      = iter ws

dropYear :: String -> [String] -> String
dropYear _ [] = ""
dropYear current years@(y:ys)
  | null years = current
  | null current = y
  | y > current = dropYear y ys
  | otherwise = dropYear current ys

-- | Get all the links in the page.
--
-- Returns a list of strings.
getLinks :: Link -> IOSLA (XIOState ()) XmlTree (NTree XNode) -> IO Links
getLinks url doc = do
  !links <- runX $! doc
    //> hasName "a"
    >>> getAttrValue "href"
    >>> arr dropBadDomains
    >>> arr (\x -> if "/"     `isSuffixOf` x || "#" `isSuffixOf` x then init x else x)
    >>> arr (\x -> if "/"     `isPrefixOf` x && not (x `isInfixOf` url) then printf "%s%s" url x else x)
    >>> arr (\x -> if "#"     `isPrefixOf` x && not (x `isInfixOf` url) then printf "%s/%s" url x else x)
    -- >>> arr (\x -> if "?"     `isPrefixOf` x && not (x `isInfixOf` url) then printf "%s%s" url x else x)
    >>> arr (\x -> if "."     `isPrefixOf` x && not (x `isInfixOf` url) then printf "%s%s" url x else x)
    >>> arr (\x -> if "http"  `isPrefixOf` x then x else printf "%s/%s" url x)
    >>> arr (\x -> if "htm"   `isSuffixOf` x || "html" `isSuffixOf` x then dropEnding x else x)
    >>> arr trim
  return $! Set.fromList $! filter (not . null) links
      where
        dropEnding :: String -> String
        dropEnding arr
          | null arr = arr
          | last arr == '.' = init arr
          | otherwise = dropEnding $! init arr

        -- | Check if string has a valid domain
        dropBadDomains :: String -> String
        -- dropBadDomains url
        --   | any (`isInfixOf` url) domains = url
        --   -- | any (`isPrefixOf` url) domains = url
        --   -- | any (`isSuffixOf` url) domains = url
        --   | otherwise                     = ""
        dropBadDomains url
          | "?" `isInfixOf` url = ""
          | "youtube.com" `isInfixOf` url || "youtu.be" `isInfixOf` url = ""
          | "twitter.com" `isInfixOf` url = ""
          | "facebook.com" `isInfixOf` url = ""
          | "google.com" `isInfixOf` url = ""
          | "amzn.to" `isInfixOf` url = ""
          | "itunes.apple.com" `isInfixOf` url = ""
          | "instagram.com" `isInfixOf` url = ""
          | "shutterstock.com" `isInfixOf` url = ""
          | "github.com" `isInfixOf` url = ""
          | "tiktok.com" `isInfixOf` url = ""
          | "linkedin.com" `isInfixOf` url = ""
          | "snapchat.com" `isInfixOf` url = ""
          | ".pdf" `isSuffixOf` url = ""
          | ".zip" `isSuffixOf` url = ""
          | otherwise = url

        trim :: String -> String
        trim = dropWhileEnd (== '/') . iter "" . dropWhileEnd isSpace . dropWhile isSpace
          where
            iter acc [] = acc
            iter acc (x:xs)
              | isSpace x       = acc
              | x == '#'   = acc
              | otherwise       = iter (acc ++ [x]) xs



-- tP1 = parse "https://wiki.haskell.org"
tP1 :: IO WebPage
-- tP1 = loadPage "https://github.com/siavava"
tP1 = loadPage "https://www.nytimes.com/2022/04/28/health/menthol-ban-fda.html"

tP2 :: IO WebPage
-- tP2 = getHTML "https://www.nytimes.com/2022/04/28/health/menthol-ban-fda.html"
tP2 = loadPage "https://github.com/siavava"
tP3 :: IO WebPage
tP3 = loadPage "https://singularityhub.com/2022/04/20/gm-just-patented-a-self-driving-car-that-teaches-people-to-drive/"


-- | Fall-back get first item of an list.
--
-- If empty list, returns an empty string.
getFirst :: [String] -> String
getFirst []    = ""
getFirst (x:_) = x

fix :: [String] -> [String]
fix [] = []
fix (x:y:xs)
  | null x        = fix (y:xs)
  | last x /= '.' = fix $! (x ++ y) : xs 
  | otherwise     = x : fix (y:xs)
fix (x:xs)
  | null x        = fix xs
  -- | last x /= '.' = if null xs then [x] else [x ++ head xs]
  | last x /= '.' = x : fix xs
  | otherwise     = x : xs
