---
schema: 1
id: clov-W44GJTYN
title: Configure Claude Code sessions in this repo to use the clove MCP server
status: closed
type: chore
priority: 2
created: 2026-11-04T09:47:32.013Z
updated: 2026-11-04T09:48:17.835Z
closed: 2026-11-04T09:48:17.835Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
This repository has no project MCP config, so a Claude Code session here does not get the clove MCP tools unless the user installs the marketplace plugin.
Add .mcp.json that registers a stdio server named clove (command clove, args [mcp]). Add .claude/settings.json that enables that server (enabledMcpjsonServers) and allows the mcp__clove__* tools. Un-ignore .claude/settings.json in .gitignore. Update CLAUDE.md to say that the clove MCP tools are the default way to track work.
Done when: both JSON files parse, git tracks .claude/settings.json, and the permission list matches the tool set in crates/clove-mcp (CloveServer::tool_names).