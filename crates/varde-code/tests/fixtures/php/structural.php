<?php
// Structural fixtures: Function (function_definition / method_declaration),
// Class, Interface, Parameter, Variable (property_element), MemberAccess, plus
// Extends (base_clause) and Implements (class_interface_clause). A `trait`
// carries implementation, so it maps to the Class kind (see REQUIRED_KINDS in
// src/extract/langs/php.rs).

namespace App\Http;

interface Greeter
{
    public function greet(string $name): string;
}

trait SomeTrait
{
    public function shared()
    {
        return 1;
    }
}

class UserController extends Base implements Greeter
{
    private int $count = 0;

    public function __construct(int $count)
    {
        $this->count = $count;
    }

    public function greet(string $name): string
    {
        return $this->count;
    }
}

function helper($x, ...$rest)
{
    return $x;
}
