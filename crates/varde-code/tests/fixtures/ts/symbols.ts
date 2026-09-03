import { helper, type Util } from "./util";
import express from "express";

const base = helper(10);

function compute(input: number): number {
  const doubled = input * 2;
  return base + doubled;
}

const app = express();

class Calculator {
  total = 0;

  add(value: number): number {
    this.total += value;
    return this.total;
  }
}
