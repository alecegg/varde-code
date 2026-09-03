<?php
// Control-flow / error fixtures: ControlFlow (if/foreach/while/switch/try/
// return/break), Catch (catch_clause), and Throw (throw_expression), plus the
// calls and member accesses that keep the fixture self-describing.

function process($items)
{
    $total = 0;
    if ($items) {
        try {
            risky();
        } catch (\Exception $e) {
            throw new RuntimeException("boom");
        }
        foreach ($items as $n) {
            $total = $total + $n;
        }
        while ($total > 0) {
            $total = $total - 1;
        }
        switch ($total) {
            case 0:
                break;
            default:
                break;
        }
    }
    return $total;
}
