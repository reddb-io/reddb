# Native compressed blob fixture

`reddb-file` owns this layout and `manifest.json`. The encoded content starts
after the ordinary page header. Only the first page may be compressed:

| Byte offset | Width | Value |
| --- | --- | --- |
| 0 | 4 | `RDBZ` |
| 4 | 4 | Next physical page ID, little endian; zero ends the chain |
| 8 | 4 | Compressed chunk length, little endian |
| 12 | 8 | Complete decoded payload length, little endian |
| 20 | 4 | Complete decoded payload CRC32, little endian |
| 24 | chunk length | First bytes of one Zstd frame |

Continuation pages keep the existing 12-byte `RDBL` header. The fixture uses
one page and a canonical `RDFT` frame. Descriptor checksums and lengths describe
decoded bytes; descriptor page counts describe physical pages. A production
reader must also reject cycles and check ordinary page checksums.

The Rust codec test checks the decoded canonical frame, hash and budget against
this manifest. Run the independent reader with:

```sh
node testdata/conformance/native-blob-compression/verify.mjs
```

Node must expose native Zstd and CRC32; missing support fails explicitly.
The independent verifier checks the manifest's header offsets, decoded length,
CRC32, SHA-256 and budget rejection. It does not replace multi-page pager or
recovery tests.

Existing `RDBL` blobs remain readable. Older engines cannot read `RDBZ`.
Qualify rollback using a compatible engine or a verified pre-upgrade backup.
