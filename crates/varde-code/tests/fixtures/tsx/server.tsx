// Realistic TSX: an Express-style server that renders a JSX component to a
// string. Covers the domain-specific kinds (Route from app.*/router.*,
// Response from res.json/res.send/res.status(n).json/res.sendStatus).

import express from "express";
import { renderToString } from "react-dom/server";
import { TodoApp } from "./component";

interface Health {
  status: string;
  uptime: number;
}

const app = express();
const router = express.Router();
const port = 3000;

app.get("/", (req, res) => {
  const html = renderToString(<TodoApp items={[]} onToggle={() => {}} />);
  res.status(200).send(html);
});

router.get("/health", (req, res) => {
  const health: Health = { status: "ok", uptime: process.uptime() };
  res.json(health);
});

app.use("/api", router);

app.post("/api/todos", (req, res) => {
  try {
    const todo = req.body;
    if (!todo.title) {
      throw new Error("missing title");
    }
    res.status(201).json(todo);
  } catch (err) {
    res.sendStatus(400);
  }
});

app.listen(port, () => {
  console.log(`listening on ${port}`);
});

export default app;
