-- Lua coverage fixture: exercises every kind in lua.rs REQUIRED_KINDS.
--
-- Import (require / require "..."): a module dependency, not a plain Call.
-- Function/Variable/Parameter: named, local, table-method, and anonymous
-- functions; local/global/multiple assignments; parameters.
-- Expression-level: Call, MemberAccess, Literal.
-- Control-flow / error: ControlFlow (if/for/while/repeat/return/break),
-- Throw (error/assert), and Catch (pcall) — Lua has no try/catch construct.

local json = require("json")
local util = require "util"

local M = {}

local counter = 0
host = "localhost"
local enabled = true
local missing = nil
local ratio = 3.5

local function add(x, y)
  return x + y
end

function M.compute(base, delta)
  local total = add(base, delta)
  return total
end

function M:method(n)
  return self.value + n
end

local doubler = function(z)
  return z * 2
end

local length = host.length
util.log("ready")

if counter > 5 then
  error("too many")
elseif counter < 0 then
  return nil
else
  counter = counter + 1
end

for i = 1, 10 do
  counter = counter + i
end

while counter > 0 do
  counter = counter - 1
end

repeat
  counter = counter + 1
until counter > 3

assert(enabled, "must be enabled")
local ok, err = pcall(function() error("boom") end)

return M
