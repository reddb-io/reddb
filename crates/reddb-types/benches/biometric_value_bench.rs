//! Decode and 1:1 comparison benchmarks for native biometric value types.
//!
//! Run with:
//! `cargo bench -p reddb-io-types --bench biometric_value_bench`

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use reddb_types::{
    value_codec, BiometricFormat, BiometricSample, BitSignature, FeatureVector, Value,
};

const SPACE: [u8; 16] = [0x44; 16];

fn iso_fmr(minutiae: usize) -> Vec<u8> {
    let length = 24 + 4 + minutiae * 6 + 2;
    let mut bytes = Vec::with_capacity(length);
    bytes.extend_from_slice(b"FMR\0 20\0");
    bytes.extend_from_slice(&(length as u32).to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes.extend_from_slice(&500_u16.to_be_bytes());
    bytes.extend_from_slice(&500_u16.to_be_bytes());
    bytes.extend_from_slice(&197_u16.to_be_bytes());
    bytes.extend_from_slice(&197_u16.to_be_bytes());
    bytes.extend_from_slice(&[1, 0]);
    bytes.extend_from_slice(&[1, 0, 100, minutiae as u8]);
    for index in 0..minutiae {
        let x = 100_u16 + (index as u16 % 16) * 40;
        let y = 120_u16 + (index as u16 / 16) * 40;
        bytes.extend_from_slice(&(x | (((index % 2) + 1) as u16) << 14).to_be_bytes());
        bytes.extend_from_slice(&y.to_be_bytes());
        bytes.extend_from_slice(&[((index * 7) % 180) as u8, 90]);
    }
    bytes.extend_from_slice(&[0, 0]);
    bytes
}

fn bench_decode(c: &mut Criterion) {
    let fingerprint =
        BiometricSample::with_format(BiometricFormat::FingerprintIso19794_2, iso_fmr(32))
            .expect("valid benchmark fingerprint");
    let feature = FeatureVector::new(SPACE, vec![0.25; 512]).expect("valid feature vector");
    let signature = BitSignature::new(SPACE, vec![0xaa; 512], None).expect("valid signature");
    let cases = [
        ("fingerprint_32_minutiae", Value::Fingerprint(fingerprint)),
        ("feature_512", Value::FeatureVector(feature)),
        ("bits_512_bytes", Value::BitSignature(signature)),
    ];
    let mut group = c.benchmark_group("biometric_value_decode");
    for (name, value) in cases {
        let mut encoded = Vec::new();
        value_codec::encode(&value, &mut encoded);
        group.throughput(Throughput::Bytes(encoded.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(name), &encoded, |b, input| {
            b.iter(|| value_codec::decode(black_box(input)).expect("decode benchmark value"))
        });
    }
    group.finish();
}

fn bench_compare(c: &mut Criterion) {
    let feature_left = FeatureVector::new(SPACE, vec![0.25; 512]).expect("valid feature vector");
    let mut feature_values = vec![0.25; 512];
    feature_values[511] = 0.5;
    let feature_right = FeatureVector::new(SPACE, feature_values).expect("valid feature vector");
    let bits_left = BitSignature::new(SPACE, vec![0xaa; 512], None).expect("valid signature");
    let bits_right = BitSignature::new(SPACE, vec![0xa8; 512], None).expect("valid signature");

    let mut group = c.benchmark_group("biometric_artifact_compare_1_to_1");
    group.throughput(Throughput::Elements(512));
    group.bench_function("feature_cosine_512", |b| {
        b.iter(|| {
            black_box(feature_left.compare(black_box(&feature_right)))
                .expect("compatible feature vectors")
        })
    });
    group.bench_function("masked_hamming_512_bytes", |b| {
        b.iter(|| {
            black_box(bits_left.compare(black_box(&bits_right))).expect("compatible bit signatures")
        })
    });
    group.finish();
}

criterion_group!(benches, bench_decode, bench_compare);
criterion_main!(benches);
