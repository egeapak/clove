---
schema: 1
id: clov-AT52NB9H
title: 'sync github: propagate comment edits both ways'
status: closed
type: feature
priority: 3
created: 2026-10-08T10:07:07.000Z
updated: 2026-11-03T17:38:52.942Z
closed: 2026-10-21T07:24:43.000Z
labels: [sync]
deps: []
relates: []
duplicates: []
supersedes: []
source_system: github
external_ref: gh-81
---
Today plan_comments (crates/clove-import/src/sync.rs:740) pulls a GitHub comment once (by id) and pushes a local comment once (by body hash). So an edit on GitHub after the pull is never pulled, and a local edit after the push is pushed as a NEW comment, leaving the old one on GitHub. Track a stable link between a local comment and its GitHub comment id, and update the other side on edit (with a conflict rule when both changed).