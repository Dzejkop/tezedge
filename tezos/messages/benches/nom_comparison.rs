use criterion::{black_box, criterion_group, criterion_main, Criterion};
use tezos_messages::p2p::binary_message::BinaryMessage;
use tezos_messages::p2p::encoding::peer::PeerMessageResponse;

fn msg() -> &'static [u8] {
    let msg = hex::decode("000000660061b12238a7c3577d725939970800ade6b82d94a231e855b46af46c37850dd02452030ffe7601035ca2892f983c10203656479cfd2f8a4ea656f300cd9d68f74aa625870f7c09f7c4d76ace86e1a7e1c7dc0a0c7edcaa8b284949320081131976a87760c300").unwrap().into_boxed_slice();
    Box::leak(msg)
}

fn serde(c: &mut Criterion) {
    let msg = msg();
    c.bench_function("nom_vs_serde::serde", |bencher| {
        bencher.iter(|| PeerMessageResponse::from_bytes(msg).unwrap())
    });
}

fn nom(c: &mut Criterion) {
    let msg = msg();
    c.bench_function("nom_vs_serde::nom", |bencher| {
        bencher.iter(|| PeerMessageResponse::parse(msg).unwrap())
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default();
    targets = nom, serde
}

criterion_main!(benches);
