export function add(a: number, b: number): number {
  return a + b;
}

export class Calculator {
  total: number = 0;

  add(value: number): number {
    this.total += value;
    return this.total;
  }
}

export interface Point {
  x: number;
  y: number;
}
