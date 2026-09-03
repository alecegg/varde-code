# Expression-level fixtures: Call, MemberAccess, Literal, Variable, and Import
# (require / require_relative).
require "json"
require_relative "greeter"

port = 8080
host = "localhost"
enabled = true
missing = nil
ratio = 3.5

total = compute(port, 10)
length = host.length
