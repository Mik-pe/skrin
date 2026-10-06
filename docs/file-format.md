# File format v1

All integers are unsigned little-endian. The file is a 32-byte header followed by zero or more transaction frames. There are no native pointers, padding bytes from Rust structs, compression, or encrypted regions. API compatibility is experimental; changing this representation requires a deliberate format-version decision and fixture update.

## File header

| Offset | Bytes | Field |
| ---: | ---: | --- |
| 0 | 8 | `SKRIN\0\r\n` magic |
| 8 | 4 | Storage format version, currently 1 |
| 12 | 8 | Application table ID |
| 20 | 4 | Application schema version |
| 24 | 4 | Reserved, must be zero |
| 28 | 4 | CRC-32 of bytes `[0, 28)` |

Table/schema versions are independent of the storage format. A wrong record identity is rejected before decoding rows or repairing a tail. A file shorter than 32 bytes is invalid, not an empty database.

## Transaction frame

Offsets below are relative to the beginning of a frame.

| Offset | Bytes | Field |
| ---: | ---: | --- |
| 0 | 4 | `TXN1` magic |
| 4 | 4 | Payload length `N`, capped at 16 MiB |
| 8 | 8 | Sequence, starting at 1, exactly previous + 1 |
| 16 | 4 | CRC-32 of the payload |
| 20 | 4 | CRC-32 of the preceding 20 frame-header bytes |
| 24 | N | Operation payload |
| 24 + N | 8 | Repeated sequence |
| 32 + N | 4 | `END!` trailer magic |

Total frame size is `36 + N` bytes. No-op transactions produce no frame and consume no sequence. The trailer is a completeness marker, not proof that a caller received success or that a disk sync happened.

## Payload

The first four bytes hold an operation count. It must be nonzero and possible within the bounded payload. Each operation starts with a one-byte tag and eight-byte primary key:

- Tag 1 (put): key, then a `u32` encoded-record length, then exactly that many record bytes.
- Tag 2 (delete): key only. A delete of an absent key has no effect.

Keys appear at most once in a frame. Writers emit key-sorted, coalesced changes. A frame may not contain unknown tags, duplicate keys, trailing payload bytes, oversized records, or a record codec that leaves bytes unconsumed. The complete set of operations is decoded before publication during recovery.

An individual encoded record is capped at 8 MiB. The built-in encoder writes `u8`, `u32`, `u64`, and length-prefixed byte/UTF-8 strings; the application decides the stable field order. Length fields are bounds-checked before slicing or application allocation. Decoder code remains trusted application code and must obey the `Record` contract.

## Checksum

CRC-32/ISO-HDLC (IEEE) uses reflected polynomial `0xedb88320`, initial state `0xffffffff`, and final bitwise inversion. The check value for ASCII `123456789` is `0xcbf43926`. This is error detection, not authentication.

`crates/skrin/tests/fixtures/format-v1.hex` freezes a file with table ID 7, schema 1, and transaction 1 putting key 42 with an eight-byte record containing integer 9. It was generated independently using Python `struct` and `zlib.crc32`; tests compare exact bytes, not only a self-round-trip.

## EOF and corruption

See [durability](durability.md) for the exact recovery policy. Only an incomplete final frame may be truncated. Bad complete headers/payloads/trailers are errors. In particular, changing a length field without fixing its checksum must not turn corruption into apparent truncation. Valid checksums do not bypass semantic validation.

Standalone v1 remains unchanged. Managed directories use separate manifest/snapshot formats and a deliberately versioned v2 WAL segment bound to a generation and snapshot sequence; see [managed storage](managed-storage.md#binary-formats). The stable directory lock survives WAL replacement. No standalone file is silently converted on open.
