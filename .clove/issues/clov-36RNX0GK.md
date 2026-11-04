---
schema: 1
id: clov-36RNX0GK
title: clove setup registers the MCP server in settings.json, which Claude Code does not read for servers
status: closed
type: bug
priority: 2
created: 2026-11-04T09:47:32.144Z
updated: 2026-11-04T16:58:27.982Z
closed: 2026-11-04T16:58:27.982Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cmd/setup.rs write_settings() writes mcpServers.clove into <project>/.claude/settings.json. Claude Code reads project MCP servers from <project>/.mcp.json (and user servers from ~/.claude.json), not from the mcpServers key of settings.json. So after clove setup the mcp__clove__* permissions exist but the server does not start.
Expected: project setup writes the server to <project>/.mcp.json and adds clove to enabledMcpjsonServers in .claude/settings.json; --global registers it where Claude Code reads user-scope servers. .claude-plugin/README.md (the clove setup paragraph) must match.
Done when: setup tests assert the new file locations and a fresh Claude Code session in a repo after clove setup lists the clove MCP server.