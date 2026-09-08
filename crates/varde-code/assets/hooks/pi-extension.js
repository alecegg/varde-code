// Injected by varde-code (see `varde-code hooks install --agent pi`).
//
// Pi extension: on session start, shells out to `varde-code nav_map`
// and returns the result as the session's system prompt so the coding agent
// starts oriented in the repo. Follows Pi's native, first-party
// `ExtensionAPI` factory function shape: `export default function(pi) {...}`
// which registers event handlers via `pi.on(...)`. No third-party
// `pi-hooks` compat shim is used.
//
// Best-effort: written against Pi's documented `ExtensionAPI` without a
// live Pi install to verify against (see the session-start-hooks plan's
// Assumptions section). If Pi's actual event name or return-value shape
// differs, this may need a follow-up update.
import { execFileSync } from "node:child_process";

export default function (pi) {
  pi.on("session_start", async (event, ctx) => {
    let navMap = "";
    try {
      const cwd = (ctx && (ctx.cwd || ctx.directory)) || process.cwd();
      navMap = execFileSync(
        "varde-code",
        ["nav_map", "--json", JSON.stringify({ repoRoot: cwd }), "--format", "text"],
        { cwd, encoding: "utf8" }
      );
    } catch (err) {
      // Best-effort: never block session start on a nav_map failure.
      console.error("varde-code nav_map hook failed:", err);
      navMap = "";
    }

    return { systemPrompt: navMap };
  });
}
