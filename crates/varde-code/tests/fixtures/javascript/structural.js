// Structural fixtures: Function, Class, Parameter.
// Note: JavaScript has no interface construct (interface_declaration is a
// TypeScript kind) — the Interface kind is carved out for javascript (see
// REQUIRED_KINDS in src/extract/langs/javascript.rs).

function greet(name) {
  return `Hello ${name}`;
}

function add(a, b = 1, ...rest) {
  const total = a + b;
  return total;
}

class Greeter {
  constructor(greeting) {
    this.greeting = greeting;
  }
}
