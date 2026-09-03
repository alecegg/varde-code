# Structural fixtures: Function (method / singleton_method), Class, Interface
# (module), Parameter, Variable, MemberAccess, plus Extends (superclass) and
# Implements (include). Ruby has no interface keyword, so `module` maps to the
# Interface kind (see REQUIRED_KINDS in src/extract/langs/ruby.rs).

module Describable
  def describe(other)
    other
  end
end

class Greeter < Base
  include Describable

  attr_reader :greeting

  def initialize(greeting = "hi")
    @greeting = greeting
  end

  def greet(other = "world", *rest, punct:, **opts)
    message = "Hello #{other}"
    message.upcase
  end

  def self.build(greeting)
    new(greeting)
  end
end
