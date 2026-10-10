module MyData.Trie (
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
  , union, (<|>)
  , intersection, (>|<)
  -- tests
  , t1, t2, t3, t4, t5, t6, t7, t8, t9, t10,
) where

import Control.DeepSeq  (deepseq)
import Control.Monad    (join, unless, when)
import Data.Char        (isAlpha, isDigit, toLower, toUpper)
import Data.List        (foldl')
import Prelude          hiding (lookup, (<*>))
import System.IO        (Handle, IOMode (ReadMode), hFlush, hGetLine, hIsEOF,
                         openFile, stdout)
import System.IO.Unsafe (unsafePerformIO)
import Text.Printf      (printf)

-- | A prefix tree.
data Trie =
  EmptyTrie -- ^ Empty trie.
  | Trie    -- ^ Non-empty trie.
    {
        value     :: Char    -- ^ The value of the node.
      , children  :: [Trie]  -- ^ The children of the node.
      , frequency :: Int
    }

-- instance Monad Trie where


instance Show Trie where
  -- | Neat show
  show = toString

-- | Dirty show
--
-- Prints syntax around Trie.
--
-- Might be easier to read back from a text file.
repr :: Trie -> String
repr EmptyTrie     = "EmptyTrie"
repr (Trie v cs _) = "(" ++ show v ++ " " ++ concatMap repr cs ++ ")"

-- | Get the size of a Trie.
--
-- @O(n)@
size :: Trie -> Int
size EmptyTrie     = 0
size (Trie _ cs _) = foldr (\c acc -> size c + acc) 1 cs


instance Eq Trie where
  (==) EmptyTrie EmptyTrie               = True
  (==) EmptyTrie _                       = False
  (==) _ EmptyTrie                       = False
  (==) (Trie v1 cs1 w1) (Trie v2 cs2 w2) = v1 == v2 && cs1 == cs2 && w1 == w2

instance Ord Trie where
  compare EmptyTrie EmptyTrie = EQ
  compare EmptyTrie _ = LT
  compare _ EmptyTrie = GT
  compare (Trie v1 cs1 w1) (Trie v2 cs2 w2) = case compare v1 v2 of
    EQ -> compare cs1 cs2
    x  -> x

-- | Check if a Trie is a leaf node.
--
-- We define a root node as a non-empty node without children.
isLeaf :: Trie -> Bool
isLeaf EmptyTrie     = True
isLeaf (Trie _ [] _) = True
isLeaf _             = False

-- | Check if a Trie is a root node.
isRoot :: Trie -> Bool
isRoot EmptyTrie       = False
isRoot (Trie '\0' _ _) = True
isRoot _               = False

isWord :: Trie -> Bool
isWord EmptyTrie = False
isWord trie      = frequency trie > 0

-- | The default root node.
--
-- Contains the NULL char `\\0` as its value.
rootTrie :: Trie
rootTrie = Trie '\0' [] 0

-- | Insert a String into a Trie.
--
-- Returns a new Trie with the modification --
-- perhaps manage that new instance.
insert :: String -> Trie -> Trie
insert str EmptyTrie  = makeRootTrie str
insert [] trie        = trie
insert str@(a:as) trie
  | null str = trie
  | isRoot trie = trie { children = iter (children trie) str }
  | value trie == a =
    if null (children trie)
      then trie { frequency = frequency trie + 1 }
      else trie { children = iter (children trie) (drop 1 str) }
  | otherwise = error "Incompatible word."
    where
      iter :: [Trie] -> String -> [Trie]
      iter tries [] = tries
      iter [] str = [makeTrie str]
      iter [t] str@(c:cs)
        | value t == c = [insert str t]
        | value t < c = [t, makeTrie str]
        | otherwise = [makeTrie str, t]
        
      iter tries@(t1:t2:ts) chars@(c:cs)
        | value t1 > c = makeTrie chars : tries
        | value t1 == c = insert chars t1 : (t2:ts)
        | value t2 == c = t1 : insert chars t2 : ts
        | value t1 < c && value t2 > c = t1 : makeTrie chars : (t2:ts)
        | otherwise = t1 : iter (t2:ts) chars

-- | Construct a rooted Trie from a String.
makeRootTrie :: String -> Trie
makeRootTrie str = insert str rootTrie

-- | Construct a non-rooted Trie from a String.
makeTrie :: String -> Trie
makeTrie []       = EmptyTrie
makeTrie (x:y:xs) = Trie x [makeTrie (y:xs)] 0
makeTrie [x]      = Trie x [] 1

-- | Find a `Trie` with the given `Char` from a list of `Trie`s.
--
-- NOTE: This function is not tail-recursive.
--
-- We also don't care about the children of the `Trie`.
lookup :: String -> Trie -> Bool
lookup [] _ = True
lookup _ EmptyTrie = False
lookup str@(c:cs) trie
  | isRoot trie = lookup str $! findTrie (children trie) str
  | value trie == c = null cs || lookup cs (findTrie (children trie) cs)
  | otherwise = False
    where
      findTrie :: [Trie] -> String -> Trie
      findTrie _ [] = EmptyTrie
      findTrie [] _ = EmptyTrie
      findTrie (t:ts) str@(c:_)
        | value t == c = t
        | value t < c = findTrie ts str
        | otherwise = EmptyTrie

-- | Trie union (infix operator)
--
-- @O(n1 + n2)@
(<|>) :: Trie -> Trie -> Trie
(<|>) = union

-- | Trie intersection (infix operator).
--
-- @O(min(n1, n2))@
(>|<) :: Trie -> Trie -> Trie
(>|<) = intersection

-- | Trie union.
--
-- @O(n1 + n2)@
union :: Trie -> Trie -> Trie
union EmptyTrie t = t
union t EmptyTrie = t
union trie1 trie2
  | value trie1 /= value trie2 = error "Incompatible Tries for union."
  | otherwise = trie1 {
      frequency = frequency trie1 + frequency trie2
    , children = iter (children trie1) (children trie2)
      }
    where
      iter :: [Trie] -> [Trie] -> [Trie]
      iter [] [] = []
      iter [] ts = ts
      iter ts [] = ts
      iter (t1:ts1) (t2:ts2)
        | value t1 == value t2 = (t1 `union` t2) : iter ts1 ts2
        | value t1 < value t2 = t1 : iter ts1 (t2:ts2)
        | otherwise = t2 : iter (t1:ts1) ts2

-- | Trie intersection.
--
-- @O(min(n1, n2))@
intersection :: Trie -> Trie -> Trie
intersection EmptyTrie t = t
intersection t EmptyTrie = t
intersection trie1 trie2
  | value trie1 /= value trie2 = error "Incompatible Tries for union."
  | otherwise = trie1 {
        frequency = min (frequency trie1) ( frequency trie2)
      , children = iter (children trie1) (children trie2)
    }
    where
      iter :: [Trie] -> [Trie] -> [Trie]
      iter [] _ = []
      iter _ [] = []
      iter (t1:ts1) (t2:ts2)
        | value t1 == value t2 = (t1 `intersection` t2) : iter ts1 ts2
        | value t1 < value t2 = iter ts1 (t2:ts2)
        | otherwise = iter (t1:ts1) ts2

-- | Print all the proper words in a `Trie`.
--
-- We do a depth-first traversal.
--
-- This function generates a sorted arrangement of the words in the Trie.
printTrie :: Trie -> IO ()
printTrie trie = putStr $! toString trie

-- | OLD: Print all the proper words in a `Trie`.
trieToStr' :: Trie -> String
trieToStr' EmptyTrie = ""
trieToStr' t
  | isRoot t = concatMap (`stringify` "") (children t)
  | otherwise = concatMap (`stringify` [value t]) (children t)
    where
      stringify :: Trie -> String -> String
      stringify EmptyTrie str = str
      stringify t str
        | isWord t = (str ++ [value t] ++ "\n") ++ concatMap (`stringify` (str ++ [value t])) (children t)
        | otherwise = concatMap (`stringify` (str ++ [value t])) (children t)

-- | NEW (Using printf)
toString :: Trie -> String
toString EmptyTrie = ""
toString t
  | isRoot t = concatMap (`stringify` "") (children t)
  | otherwise = concatMap (`stringify` [value t]) (children t)
    where
      stringify :: Trie -> String -> String
      stringify EmptyTrie str = str
      stringify t str
        | isWord t = printf "%5d %s%c\n%s" (frequency t) str (value t) $! concatMap (`stringify` printf "%s%c" str (value t)) (children t)
        | otherwise = concatMap (`stringify` printf "%s%c" str (value t)) (children t)

loadFile :: FilePath -> IO Trie
loadFile fp = do
  text <- readFile fp
  return $! foldr insert EmptyTrie $! filter (not . null) . map clean $! words text

dumpToFile :: Trie -> FilePath -> IO ()
dumpToFile t fp = writeFile fp $! show t


---- TEXT OPS -----
-- | Clean up the given string by removing all non-alphabetical characters
clean :: String -> String
clean = lowercase . filter isAlpha

-- | Check if char is puctuation
ispunct :: Char -> Bool
ispunct c = c `elem` "\n!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~‘“’”—…©°–"

-- | Check if char is space
isspace :: Char -> Bool
isspace c = c `elem` " \t\r\f\v"

-- | Check if char is alphabetical, i.e. neither punct nor space
isalpha :: Char -> Bool
isalpha c = not $! isspace c || ispunct c || isDigit c

lowercase, uppercase :: String -> String
-- | Convert string to lowercase
lowercase = map toLower

dropLines :: String -> String
dropLines = filter (/= '\n')

-- | convert string to uppercase
uppercase = map toUpper

-- simple tests

writeToFile :: FilePath -> Trie -> IO ()
writeToFile fp trie = writeFile fp $! toString trie

t1, t2, t3, t4, t5, t6, t7, t8, t9, t10 :: Trie
t1 = makeRootTrie "abc"
t2 = insert "abd" t1
t3 = insert "xyz" t2
t4 = insert "abcde" t3
t5 = insert "amittai" t4
t6 = insert "joel" t5
t7 = insert "siavava" t6
t8 = insert "what" t7
t9 = insert "is" $ insert "this" $ insert "haha" $ insert "okay" $ insert "abcdef" t8
t10 = insert "also" t9

-- t11 :: IO Trie
-- t11 = loadFile "../../data/simpletext"

t11 :: IO Trie
t11 = loadFile "../../data/metadata/dictionary"

