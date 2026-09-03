function greet(name: string): string {
  return `Hello ${name}`;
}

class Greeter {
  greeting: string;

  constructor(greeting: string) {
    this.greeting = greeting;
  }
}

interface Point {
  x: number;
  y: number;
}
