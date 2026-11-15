---
schema: 1
id: clov-6JP2HWY7
title: Web detail sidebar and form controls have no accessible name
status: closed
type: bug
priority: 2
created: 2026-11-14T19:13:01.295Z
updated: 2026-11-14T19:13:01.696Z
closed: 2026-11-14T19:13:01.696Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
In crates/clove-web/web/src/routes/items/[id]/+page.svelte the Status and Priority selects, the assignee input, the label input, the dependency input and the comment textarea sit under a div.side-label or carry only a placeholder. In src/lib/components/ItemForm.svelte the Labels input sits under a <span> and the body textarea has no label. The same holds for the dependency and parent inputs of routes/items/[id]/edit/+page.svelte. A screen reader announces them without a name.
Done when: 'detail sidebar controls have accessible names' in src/test/item-detail-states.svelte.test.ts passes.