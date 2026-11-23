# wsp-objects

The hub's object format (wsp-458; the multiplayer design's "Hash-based sync, and why it scales", decisions d5
and d6). Pure: no IO, no clock. Depends on `wsp-store-contract` (the entities, subjects, kinds and principals an
object carries), `sha2`, `serde` and `serde_json`, nothing else in the workspace. `wsp-api`'s sync verbs carry its
ids, refs and workspaces, and `wsp-client`'s sync verifies and diffs with it (wsp-459); the hub's object and ref
store is wsp-460, a spoke's wsp-464 and wsp-469.

## Holds

- `src/lib.rs` — `WorkspaceId` (a `String` newtype until wsp-168 lands the contract's); `ObjectId`, the SHA-256
  of an object's canonical bytes, written `sha256:<hex>` in text and serde; `Object`, `Record` (one version of an
  `Entity`) or `Event` (a kind and its payload), each with `workspace`, `subject`, `parent`, `by`, `at`;
  `encode` (canonical bytes and their id), `decode` (any JSON of the shape), `verify` (the bytes hash to the id
  and are canonical; schema-free, so an older store keeps a newer writer's objects), `canonical` (any JSON
  text, written canonically); `Refs` (one workspace's subject → head, serialized as a list since a subject
  cannot key a JSON map) and `diff` (`we_lack`, `they_lack`, `differ`; refused across workspaces); `Error`
- `tests/objects.rs` — the same object encodes to the same id; reordered keys keep it; any changed byte changes
  it; `verify` refuses a wrong hash and non-canonical bytes; an unknown kind verifies and decodes as `Unknown`;
  the id's one text form; `diff` names exactly what each side lacks and what differs; `Refs` round-trips

## Contract

- No update API: a change is a new object whose `parent` is the old one.
- Canonical JSON: UTF-8, no insignificant whitespace, keys sorted by their UTF-8 bytes (here, not by
  `serde_json`'s map, whose order `preserve_order` would change), every field present (`parent: null`).
