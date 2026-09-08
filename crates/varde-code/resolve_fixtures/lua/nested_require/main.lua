-- `require "foo.bar"` uses Lua's dotted module path -> foo/bar.lua.
local bar = require("foo.bar")
-- `require "util"` names a directory package -> util/init.lua.
local util = require("util")

local function run()
  return bar.helper() + util.now()
end

return run
