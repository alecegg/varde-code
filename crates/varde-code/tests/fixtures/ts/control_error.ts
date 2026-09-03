function process(items: string[]): string {
  try {
    if (items.length === 0) {
      throw new Error("empty");
    }
    for (const item of items) {
      while (item.length > 10) {
        console.log(item);
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
