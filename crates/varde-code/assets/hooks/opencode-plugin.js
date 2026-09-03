// Injected by varde-code (see `varde-code hooks install --agent opencode`).
//
// opencode plugin: on session start, shells out to `varde-code report
// nav_map` and injects the result into the session as context so the
// coding agent starts oriented in the repo. Follows opencode's documented
// plugin function shape: an async function receiving the plugin context
// (`project`, `client`, `$`, `directory`, `worktree`) and returning an
// object of hook handlers.
//
// Best-effort: written against opencode's documented plugin API without a
// live opencode install to verify against (see the session-start-hooks
// plan's Assumptions section). If opencode's actual hook name or context
// injection mechanism differs, this may need a follow-up update.
export const VardeCodeNavMap = async ({ $, directory }) => {
  return {
    event: async ({ event }) => {
      if (event.type !== "session.start") return;

      let navMap;
      try {
        navMap = await $`varde-code report nav_map --json ${JSON.stringify({
          repoRoot: directory,
        })} --format text`.cwd(directory).text();
      } catch (err) {
        // Best-effort: never block session start on a nav_map failure.
        console.error("varde-code nav_map hook failed:", err);
        return;
      }

      if (!navMap || !navMap.trim()) return;

      await event.client?.session?.messages?.append?.({
        role: "system",
        content: navMap,
      });
    },
  };
};
