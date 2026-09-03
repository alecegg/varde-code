module Control (classify, risky, safely) where

-- Guards + if + case -> ControlFlow. error -> Throw.
classify :: Int -> String
classify n
  | n < 0 = "negative"
  | n == 0 = branch
  | otherwise = fallback
  where
    branch = case n of
      1 -> "one"
      _ -> if n > 100 then "huge" else "small"
    fallback = "other"

-- error / throwIO -> Throw.
risky :: Int -> Int
risky x = error "boom"

-- catch / handle -> Catch (library combinators).
safely :: IO ()
safely = catch action handler

action :: IO ()
action = doWork 3

handler :: IO ()
handler = recover 0
