<?php
// Import source: `require_once "b.php"` resolves to sibling b.php via the
// shared file-stem matcher, and the call below binds cross-file to helper().
require_once "b.php";

function run()
{
    return helper(41);
}
