use criterion::{criterion_group, criterion_main, Criterion};
use tezos_messages::p2p::binary_message::BinaryMessage;
use tezos_messages::p2p::encoding::peer::PeerMessageResponse;

fn msg() -> Vec<u8> {
    hex::decode("0000027300613158c8503e7cd436d09a8a6320cd57014870a96f178915be25551e435d0830ab00f0f0007c09f7c4d76ace86e1a7e1c7dc0a0c7edcaa8b284949320081131976a87760c30a37f18e2562ae14388716247be0d4e451d72ce38d1d4a30f92d2f6ef95b4919000000658a7912f9de23a446748861d2667ffa3b4463ed236689492c74703cef598e6f3f0000002eb6d1852a1f397619b16f08121fb01d43a9bf4ded283ab0d96fd114028251690506a7ec514f0b297b6cdc8ff54a658f27f7635d201c61479cd48007c0096752fb0c000000658a7912f9de23a446748861d2667ffa3b4463ed236689492c74703cef598e6f3f0000002eb62b8768820e6b7343c32382544d0fa0f044289fd1b86ee5c66e36396bc9bc2492314543667770959449943d222ffd7f7cd8e3ad8eda9d21a8a5e9e34c73c0c9e3000000658a7912f9de23a446748861d2667ffa3b4463ed236689492c74703cef598e6f3f0000002eb6c5d4ac0ba67f6509fec4ae196d1cb7ccf8ee7a35bc06d362d69291631a5a07b511252c70d59ff94dc4071525dd6c22354349702c9821d80c748a15913f11b1d1000000658a7912f9de23a446748861d2667ffa3b4463ed236689492c74703cef598e6f3f0000002eb63d61de83c6f71ca631903f29be9040f63dbf5d00d7994a8420210270aa2c37e245ce70e8f4d7d384f342f7e6b6797c5f237ae1846a8b8652838663d1d0df91a0000000658a7912f9de23a446748861d2667ffa3b4463ed236689492c74703cef598e6f3f0000002eb6c69c651e14357c3a895cd6465fc1e3b1fd19b0d805efae484f2632e006101b9c80c28c92dcfbf58b99392b2108b286fd28039ddd72294929c2fbf9dda65acf01").unwrap()
}

fn serde(c: &mut Criterion) {
    let msg = msg();
    c.bench_function("nom_vs_serde::serde", |bencher| {
        bencher.iter(|| PeerMessageResponse::from_bytes(&msg).unwrap())
    });
}

fn nom(c: &mut Criterion) {
    let msg = msg();
    c.bench_function("nom_vs_serde::nom", |bencher| {
        bencher.iter(|| PeerMessageResponse::parse(&msg).unwrap())
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default();
    targets = nom, serde
}

criterion_main!(benches);
