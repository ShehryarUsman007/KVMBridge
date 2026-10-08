# `.kvmb` — KVM Bridge configuration file format (version 1)

A `.kvmb` file is a **backup** of one computer's configuration. It is a
small binary container of typed sections. Section payloads are UTF-8 JSON so they stay readable when the
file is not encrypted and tolerate added fields. All integers are big-endian.

Implementation: `crates/core/src/configfile.rs`.

## Header (16 bytes)

| Offset | Size | Field        | Value                                             |
|-------:|-----:|--------------|---------------------------------------------------|
| 0      | 8    | magic        | ASCII `KVMBCONF`                                  |
| 8      | 2    | version      | `1`                                               |
| 10     | 2    | flags        | bit 0 = encrypted, bit 1 = contains identity      |
| 12     | 4    | body length  | number of bytes after the header                  |

Readers reject files whose `version` is newer than they understand, and files
whose total length is not `16 + body length`.

## Body — unencrypted (flag bit 0 clear)

```
sections ... || checksum
```

`checksum` is BLAKE2s-256 over `header || sections` (32 bytes). It detects
accidental damage; it is not a security mechanism. Unencrypted files must not
contain an identity section.

## Body — encrypted (flag bit 0 set)

| Size | Field                                              |
|-----:|----------------------------------------------------|
| 1    | KDF id: `1` = Argon2id (v0x13)                     |
| 4    | memory cost in KiB (writer uses 65536 = 64 MiB)    |
| 4    | time cost / passes (writer uses 3)                 |
| 1    | parallelism (writer uses 1)                        |
| 16   | salt (random)                                      |
| 1    | cipher: `1` = AES-256-GCM, `2` = ChaCha20-Poly1305 |
| 12   | nonce (random)                                     |
| n+16 | AEAD ciphertext of `sections`, followed by the tag |

The 32-byte key is `Argon2id(password, salt, m, t, p)`. The associated data
is the header together with all of the parameter bytes above, so changing
the flags or the parameters makes decryption fail. The cipher defaults to the
one configured in KVM Bridge. Readers refuse unreasonable KDF parameters
(memory over 1 GiB, more than 16 passes or lanes), so a crafted file cannot
exhaust memory.

## Sections

```
u16 type || u32 length || data[length]
```

| Type | Content                                                                       |
|-----:|-------------------------------------------------------------------------------|
| 1    | **info** (required): `{created_ms, app_version, source_id, source_name, source_os, note}` |
| 2    | **settings**: the device settings object (same shape as in `config.json`)    |
| 3    | **shared settings**: `{rev, author, cipher, common}` (`common` since 0.1.4: the settings all paired computers share) |
| 4    | **layout**: `{rev, author, slots: [{device, col, row, number, name}], controller}` |
| 5    | **paired devices**: `[{id, name, public_key (hex), last_addr, paired_at_ms}]` |
| 6    | **identity** (binary, encrypted files only): 16-byte device UUID followed by the 32-byte X25519 private key |
| 7    | *saved config* (before 0.1.4; skipped now)                                   |

Readers skip section types they do not know, so future versions can add
sections without breaking older readers.

## Import rules

* **Settings**: this computer keeps its own name, unless the identity is
  restored as well.
* **Layout**: the slot of the computer that saved the file is mapped to this
  computer. Slots for devices that are not paired here are dropped, and the
  result is limited to 5 screens.
* **Paired devices**: added or replaced, up to the limit of 5 computers. The
  other computers still have to trust *this* computer's key, which means one
  of these holds:
  * the file came from this same computer;
  * the identity is restored too;
  * the devices pair again. Because this side already trusts them, it
    confirms their pairing automatically.
* **Identity**: replaces this computer's device id and private key, then
  restarts KVM Bridge.
