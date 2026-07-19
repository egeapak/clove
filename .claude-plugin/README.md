# clove Plugin for Claude Code

Give your coding agent native access to your project's work items. This plugin
exposes [clove](https://github.com/egeapak/clove) — a fast, git-native,
dependency-aware work-item tracker — to Claude Code as an MCP server, so the
agent can browse, search, and update issues, and reason about the dependency
graph (what's ready, what's blocked) directly from your repository.

## Prerequisites

The `clove` binary (and, recommended, the `cloved` daemon) must be installed and
on your `PATH`:

```bash
cargo install --git https://github.com/egeapak/clove clove-cli cloved
```

Verify with:

```bash
clove version
```

The plugin's MCP server runs `clove mcp`, which discovers the `.clove/`
repository by walking up from the project directory. The repository must already
be initialized:

```bash
clove init            # creates .clove/ in the current repo (once)
```

If `.clove/` is missing, the MCP server starts but its tools return a "no clove
repository" error until you run `clove init`.

## Installation

### From the marketplace

```bash
# Add the marketplace (once)
/plugin marketplace add egeapak/clove

# Install
/plugin install clove@clove
```

### Update

```bash
/plugin update clove@clove
```

## What You Get

### MCP Server

A clove MCP server (`clove mcp`) starts automatically per session over stdio,
providing tools for work-item management. They surface in Claude Code as
`mcp__plugin_clove_tracker__*`:

- **Read:** `clove_list`, `clove_ready`, `clove_blocked`, `clove_show`,
  `clove_search`, `clove_dep_tree`, `clove_stats`
- **Write:** `clove_new`, `clove_edit`, `clove_status`, `clove_comment`,
  `clove_dep_add`, `clove_dep_remove`, `clove_set_parent`

### Daemon-coordinated writes (topology B)

Reads compute directly from the file store. Writes prefer the single `cloved`
daemon, which `clove mcp` **auto-starts** (with a heartbeat that keeps it alive
for the session) so multiple agents working on one project share one write
coordinator and stay coherent. If `cloved` isn't on your `PATH` or can't start,
writes fall back to direct file access — nothing fails because of it, though
installing `cloved` is recommended for concurrent use.

## Making the agent use clove by default

The MCP server ships **instructions** (loaded automatically when the plugin
connects) that tell the agent to treat clove as the source of truth for work
items — check `clove_ready` before starting a task, search for existing items,
and record progress with `clove_new` / `clove_status` / `clove_comment`. No
setup is required for this nudge.

To make it a **standing directive** in a project, drop a short `CLOVE.md`
(see the copy shipped in this repo's root) alongside your `CLAUDE.md` and add a
line importing it:

```
@CLOVE.md
```

Claude Code loads `@`-imported files into every session, so the agent reaches
for clove by default rather than ad-hoc TODO lists. `clove agent-doc` prints the
full command reference if you want a longer, versioned doc instead.

## Notes

- **Scope:** each Claude Code session runs its own `clove mcp` process, scoped to
  the project it's launched in.
- **Permissions:** to avoid per-call approval prompts, allow the
  `mcp__plugin_clove_tracker__*` tools in your `settings.json`.
- This plugin ships **no hooks** (clove has no hook subcommand) — it is purely an
  MCP server.

## Troubleshooting

**Tools error with "no clove repository":** run `clove init` in the project root.

**Writes not coordinating across agents:** ensure `cloved` is installed and on
`PATH` (`cargo install --git https://github.com/egeapak/clove cloved`); without
it, each session writes directly to files.

**Plugin not loading:** ensure `clove` is on your `PATH` (`clove version`).
