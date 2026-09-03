// Expression-level fixtures: Literal, Call, MemberAccess, Variable.

const port = 8080;
const host = "localhost";
const enabled = true;
const missing = null;
const nothing = undefined;
const message = `Port ${port}`;
const pattern = /ab+c/;

const total = compute(port, 10);
const length = config.resolve(host).length;
