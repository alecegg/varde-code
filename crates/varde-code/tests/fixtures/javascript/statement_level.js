// Statement-level fixtures: Variable (lexical/variable declarations) and
// Export (export statements).

const greeting = "hello";
let counter = 0;
var legacy;
const a = 1, b = 2;

export function add(x, y) {
  return x + y;
}

export const multiplier = 2;

export { counter, legacy };
