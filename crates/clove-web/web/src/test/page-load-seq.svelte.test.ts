// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { store, startLive, stopLive } from "$lib/store.svelte";
import { item, META } from "./fake-api";

// A page load: the route declares its query (`setQuery` loads the window and
// `/meta`), the layout's `startLive` makes sure `/meta` is loaded and opens
// the live socket, and the socket's `hello` reports the server's change
// sequence. The server reports the same sequence as `_meta.seq`, read before
// the data, so the page can tell whether its window missed a change.

const ID = "proj-7af3q2k9";

class FakeSocket {
    static OPEN = 1;
    static CONNECTING = 0;
    static last: FakeSocket | null = null;
    readyState = FakeSocket.CONNECTING;
    onopen: (() => void) | null = null;
    onmessage: ((ev: { data: string }) => void) | null = null;
    onclose: (() => void) | null = null;
    onerror: (() => void) | null = null;
    constructor(public url: string) {
        FakeSocket.last = this;
    }
    close() {}
}

/** The server: its current change sequence and the one item's title. */
let server: { seq: number | null; title: string };
let calls: string[];

function stubServer() {
    calls = [];
    vi.stubGlobal("fetch", async (input: string | URL) => {
        const url = new URL(String(input), "http://localhost");
        const path = url.pathname.slice(
            url.pathname.indexOf("/api/v1") + "/api/v1".length,
        );
        calls.push(path);
        const seq = server.seq === null ? {} : { seq: server.seq };
        const [data, meta] =
            path === "/meta"
                ? [META, seq]
                : path === "/items"
                  ? [
                        [item({ id: ID, title: server.title })],
                        {
                            total: 1,
                            returned: 1,
                            offset: 0,
                            limit: 100,
                            ...seq,
                        },
                    ]
                  : (() => {
                        throw new Error("unexpected request " + path);
                    })();
        return new Response(
            JSON.stringify({ v: 1, ok: true, data, _meta: meta }),
            {
                status: 200,
                headers: { "content-type": "application/json" },
            },
        );
    });
}

/** The page load up to the socket opening and greeting. */
async function loadPage(helloSeq: number) {
    store.setQuery({ limit: 100 });
    await startLive();
    const sock = FakeSocket.last!;
    sock.readyState = FakeSocket.OPEN;
    sock.onopen!();
    sock.onmessage!({
        data: JSON.stringify({
            event: "hello",
            data: { protocol: 1, source: "standalone", seq: helloSeq },
        }),
    });
    await vi.waitFor(() => expect(store.loaded).toBe(true));
    // Let a resync, if one was started, run to the end.
    for (let i = 0; i < 10; i++) await new Promise((r) => setTimeout(r, 0));
    await store.settled();
}

const count = (path: string) => calls.filter((c) => c === path).length;

beforeEach(() => {
    stopLive();
    store.resetForTests();
    store.meta = null;
    vi.stubGlobal("WebSocket", FakeSocket);
    server = { seq: 3, title: "Before" };
    stubServer();
});

afterEach(() => stopLive());

describe("a full page load", () => {
    it("makes one items request and one meta request when nothing changed", async () => {
        await loadPage(3);

        expect(count("/items")).toBe(1);
        expect(count("/meta")).toBe(1);
        expect(store.all.map((i) => i.title)).toEqual(["Before"]);
        expect(store.conn).toBe("live");
    });

    it("resyncs and shows a write that landed before the socket opened", async () => {
        // The window is read at seq 3; a write is announced (seq 4) before the
        // socket says hello, so the window is stale.
        store.setQuery({ limit: 100 });
        await startLive();
        await vi.waitFor(() => expect(store.loaded).toBe(true));
        server = { seq: 4, title: "After" };
        const sock = FakeSocket.last!;
        sock.readyState = FakeSocket.OPEN;
        sock.onopen!();
        sock.onmessage!({
            data: JSON.stringify({
                event: "hello",
                data: { protocol: 1, source: "standalone", seq: 4 },
            }),
        });

        await vi.waitFor(() =>
            expect(store.all.map((i) => i.title)).toEqual(["After"]),
        );
        await store.settled();
        // The load and the resync; the layout's meta load shared the route's.
        expect(count("/items")).toBe(2);
        expect(count("/meta")).toBe(2);
    });

    it("still resyncs against a server that reports no seq", async () => {
        server = { seq: null, title: "Before" };
        await loadPage(0);

        expect(count("/items")).toBe(2);
    });
});
