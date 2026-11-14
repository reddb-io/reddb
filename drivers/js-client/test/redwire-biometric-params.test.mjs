import assert from 'node:assert/strict'
import test from 'node:test'

import { Features, ValueTag, encodeValue } from '../src/redwire-core.js'

test('biometric feature capability and tags remain stable', () => {
  assert.equal(Features.BIOMETRIC_PARAMS, 0x08)
  assert.equal(Features.LANDMARK_PARAMS, 0x10)
  assert.equal(ValueTag.Fingerprint, 0x0a)
  assert.equal(ValueTag.Face, 0x0b)
  assert.equal(ValueTag.Iris, 0x0c)
  assert.equal(ValueTag.FeatureVector, 0x0d)
  assert.equal(ValueTag.BitSignature, 0x0e)
  assert.equal(ValueTag.LandmarkSet, 0x0f)
})

test('landmark set envelope uses structured RedWire layout', () => {
  const encoded = encodeValue({
    $landmark_set: {
      space_id: '00112233445566778899aabbccddeeff',
      dimensions: 2,
      pairs: [
        { left: [-1, 1], right: [1, 1] },
        { left: [-2, -1], right: [2, -1] },
      ],
      midline: [[0, 2]],
    },
  })
  assert.equal(encoded[0], ValueTag.LandmarkSet)
  assert.equal(encoded[17], 2)
  assert.equal(new DataView(encoded.buffer).getUint32(18, true), 2)
  assert.equal(new DataView(encoded.buffer).getUint32(22, true), 1)
})

test('feature vector and bit signature envelopes use structured RedWire layout', () => {
  const spaceId = '00112233445566778899aabbccddeeff'
  const feature = encodeValue({
    $feature_vector: { space_id: spaceId, values: [1, -2] },
  })
  assert.equal(feature[0], ValueTag.FeatureVector)
  assert.equal(feature.length, 1 + 16 + 4 + 8)
  assert.equal(new DataView(feature.buffer).getUint32(17, true), 2)

  const bits = encodeValue({
    $bit_signature: { space_id: spaceId, bits: 'aa55', mask: 'ff0f' },
  })
  assert.equal(bits[0], ValueTag.BitSignature)
  assert.deepEqual(Array.from(bits.slice(-4)), [0xaa, 0x55, 0xff, 0x0f])
})

test('biometric modality must agree with its standard', () => {
  assert.throws(
    () => encodeValue({
      $biometric: {
        modality: 'face',
        format: 'ISO/IEC 19794-2',
        data: '464d5200',
      },
    }),
    (error) => error.code === 'UNSUPPORTED_PARAM',
  )
})
