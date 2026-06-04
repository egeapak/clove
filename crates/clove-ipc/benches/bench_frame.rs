//! IPC frame-codec microbenchmark (T-X01, M3). The framing + JSON (de)serialize
//! round-trip is the per-request floor under the daemon's < 5ms QUERY budget
//! (M3-G01); the end-to-end socket round-trip is gate-tested in
//! `cloved/tests/daemon_ipc.rs`. CI compiles this with `cargo bench --no-run`.

use std::io::Cursor;

use clove_ipc::frame::{read_message, write_message};
use clove_ipc::{QueryKind, QueryListResponse, QueryRequest, Request, Response};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn sample_request() -> Request {
    Request::Query(QueryRequest {
        kind: QueryKind::List,
        status: None,
        item_type: None,
        priority: None,
        assignee: None,
        label: None,
        offset: 0,
        limit: Some(100),
    })
}

fn sample_response() -> Response {
    let rows = (0..100)
        .map(|i| clove_ipc::LeanRow {
            id: format!("proj-{i:08}"),
            status: "open".to_owned(),
            item_type: "feature".to_owned(),
            priority: 1,
            title: format!("item {i}"),
        })
        .collect();
    Response::QueryList(QueryListResponse {
        rows,
        total: 100,
        warnings: Vec::new(),
    })
}

fn bench_frame(c: &mut Criterion) {
    c.bench_function("frame_request_round_trip", |b| {
        let req = sample_request();
        b.iter(|| {
            let mut buf = Vec::new();
            write_message(&mut buf, black_box(&req)).unwrap();
            let mut cur = Cursor::new(buf);
            let got: Request = read_message(&mut cur).unwrap();
            black_box(got);
        });
    });

    c.bench_function("frame_response_100_rows_round_trip", |b| {
        let resp = sample_response();
        b.iter(|| {
            let mut buf = Vec::new();
            write_message(&mut buf, black_box(&resp)).unwrap();
            let mut cur = Cursor::new(buf);
            let got: Response = read_message(&mut cur).unwrap();
            black_box(got);
        });
    });
}

criterion_group!(benches, bench_frame);
criterion_main!(benches);
