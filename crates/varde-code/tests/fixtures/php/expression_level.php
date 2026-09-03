<?php
// Expression-level fixtures: Call (function_call_expression), MemberAccess
// (member_call / member_access), Literal (int/float/string/bool/null),
// Variable (assignment_expression), and Import (use / require).

use App\Models\User;
require_once "helper.php";

$port = 8080;
$host = "localhost";
$enabled = true;
$missing = null;
$ratio = 3.5;

$total = compute($port, 10);
$length = $host->length();
$name = $user->name;
