// @vitest-environment jsdom
import { describe, it, expect, afterEach } from "vitest";
import { render, cleanup } from "@testing-library/svelte";
import DepTree from "$lib/components/DepTree.svelte";
import type { DepTreeNode } from "$lib/types";

afterEach(cleanup);

function node(
    id: string,
    title: string,
    extra: Partial<DepTreeNode> = {},
): DepTreeNode {
    return {
        id,
        title,
        status: "open",
        ready: false,
        cycle_ref: false,
        repeat_ref: false,
        children: [],
        ...extra,
    };
}

describe("the dependency tree", () => {
    it("marks a subtree shown elsewhere as a repeat, not as a leaf", () => {
        // Diamond: A -> {B, C} -> D -> E. The server expands D under B and marks
        // the D under C `repeat_ref` with no children.
        const e = node("proj-EEEEEEEE", "Eee", { ready: true });
        const tree = node("proj-AAAAAAAA", "Aaa", {
            children: [
                node("proj-BBBBBBBB", "Bbb", {
                    children: [node("proj-DDDDDDDD", "Ddd", { children: [e] })],
                }),
                node("proj-CCCCCCCC", "Ccc", {
                    children: [
                        node("proj-DDDDDDDD", "Ddd", { repeat_ref: true }),
                    ],
                }),
            ],
        });
        const { container } = render(DepTree, { node: tree });
        const repeats = container.querySelectorAll(".repeat");
        expect(repeats.length).toBe(1);
        expect(repeats[0].textContent).toBe("(repeat)");
        // The marker sits on the second D line, not on the expanded one.
        const lines = Array.from(container.querySelectorAll(".line"));
        const dLines = lines.filter((l) => l.textContent?.includes("Ddd"));
        expect(dLines.length).toBe(2);
        expect(dLines[0].querySelector(".repeat")).toBeNull();
        expect(dLines[1].querySelector(".repeat")).not.toBeNull();
        expect(container.querySelectorAll(".cycle").length).toBe(0);
    });

    it("shows a cycle as a cycle even if it is also a repeat", () => {
        const tree = node("proj-AAAAAAAA", "Aaa", {
            children: [
                node("proj-BBBBBBBB", "Bbb", {
                    cycle_ref: true,
                    repeat_ref: true,
                }),
            ],
        });
        const { container } = render(DepTree, { node: tree });
        expect(container.querySelectorAll(".cycle").length).toBe(1);
        expect(container.querySelectorAll(".repeat").length).toBe(0);
    });
});
