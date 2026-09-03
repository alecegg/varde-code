module A (run) where

-- `import B` — the import spec `B` matches the sibling file stem `B.hs`,
-- exercising a genuinely RESOLVED import edge through resolve.rs's shared
-- stem-matching. Haskell imports are module/package-based and in general do
-- NOT stem-match files (a module name need not mirror a file path); this
-- fixture is deliberately crafted (module name == sibling file stem) so the
-- edge resolves.
import B

run :: Int
run = work
