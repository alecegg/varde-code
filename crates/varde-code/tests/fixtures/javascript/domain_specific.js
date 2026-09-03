// Domain-specific fixtures: Express-style Route (`app.get("/path", ...)`) and
// Response (`res.status(200).json(...)`, `res.send(...)`, `res.sendStatus(n)`).

import express from "express";

const app = express();
const router = express.Router();

app.get("/users", (req, res) => {
  res.status(200).json({ users: [] });
});

app.post("/users", (req, res) => {
  res.status(201).json(req.body);
});

router.get("/health", (req, res) => {
  res.send("ok");
});

app.use("/auth", authRouter);

router.delete("/users/:id", (req, res) => {
  res.sendStatus(404);
});
