module Shapes (Shape(..), area, describe) where

import Data.List (sort)
import qualified Data.Map as M

-- A typeclass -> Interface.
class Named a where
  name :: a -> String

-- data / newtype / type -> Class.
data Shape = Circle Double | Rect Double Double

newtype Radius = Radius Double

type Point = (Double, Double)

-- instance -> Implements.
instance Named Shape where
  name s = "shape"

-- A function with parameters -> Function + Parameter, application -> Call,
-- literals -> Literal.
area :: Shape -> Double
area shape = compute shape 2

-- A qualified access -> MemberAccess (and Call).
lookupPoint :: Int -> Maybe Point
lookupPoint k = M.lookup k pointTable

-- A top-level value bind -> Variable, with literals.
defaultRadius :: Double
defaultRadius = 1.0

label :: String
label = "origin"

pointTable :: M.Map Int Point
pointTable = M.empty
