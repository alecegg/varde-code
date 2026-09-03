// Control-flow / error fixtures: ControlFlow (if/while/for/match/try/return),
// Catch (`catch { case e => }`), and Throw (`throw new ...`), plus calls and
// literals to keep the fixture self-describing.

object Processor {
  def process(items: List[Int]): String = {
    var total = 0
    if (items.isEmpty) {
      return "none"
    }
    for (item <- items) {
      while (total < item) {
        total = total + 1
      }
    }
    val label = total match {
      case 0 => "zero"
      case _ => "some"
    }
    try {
      throw new IllegalStateException("boom")
    } catch {
      case e: Exception => println(e)
    }
    label
  }
}
