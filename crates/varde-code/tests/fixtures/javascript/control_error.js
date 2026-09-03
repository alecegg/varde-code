// Control-flow / error fixtures: ControlFlow, Catch, Throw.

function process(items) {
  try {
    if (items.length === 0) {
      throw new Error("empty");
    }
    for (const item of items) {
      while (item > 10) {
        item = item - 1;
        break;
      }
    }
  } catch (err) {
    return `failed: ${err}`;
  } finally {
    cleanup();
  }
}

switch (mode) {
  case "a":
    break;
  default:
    do {
      step();
    } while (running);
}
