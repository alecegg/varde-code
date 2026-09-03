# Control-flow / error fixtures: ControlFlow (if/while/case/begin/return/break/
# next), Catch (rescue), and Throw (raise), plus calls and member accesses to
# keep the fixture self-describing.

def process(items)
  begin
    raise ArgumentError, "empty" if items.empty?
    items.each do |item|
      next if item.zero?
      while item > 10
        item -= 1
        break
      end
    end
  rescue ArgumentError => err
    return err.message
  ensure
    cleanup
  end

  case items.size
  when 0
    "none"
  else
    "some"
  end
end
