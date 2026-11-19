# Embedded protobuf schemas

Status: complete for FileMaker Pro 26.0.3.307 (Mac) and 26.0.3.307a (Windows).
Regenerated `.proto` files: [protobuf/](protobuf/).

## Summary

FileMaker Pro 26 embeds exactly two FileMaker-specific protobuf packages. All
eleven files were recovered and regenerate their embedded descriptors byte for
byte.

| Package | Files | Binaries | Runtime | Purpose (confidence) |
|---|---|---|---|---|
| `FMLayoutPB` | `FMLayout.proto`, `LayoutObject.proto`, `Style.proto`, `Theme.proto` | FMEngine (Mac x86_64/arm64, Win `FMEngine.dll`) | C++ libprotobuf, proto2 | On-disk encoding of layouts, layout parts, layout objects, styles, themes and conditional formatting inside `.fmp12` files (high) |
| `eap.v2` | `service.proto`, `common.proto`, `entity.proto`, `entitydef.proto`, `query.proto`, `events.proto`, `file.proto` | DBEngine (Mac x86_64/arm64, Win `DBEngine.dll`), Mac `fmxdbc_listener` | C++ libprotobuf + gRPC C++ 1.38.0, proto3 | gRPC client for the Claris Studio data source ("Entity Access Pipeline", codename Magnolia) at `eap.platform.claris.com` (high) |

Key conclusions for OpenMaker:

- Neither package is the client/server wire protocol on port 5003. The only
  FileMaker code that opens a gRPC channel is `Draco::DAL_gRPC`, the Claris
  Studio data source driver (high confidence). Port 5003 is therefore not
  gRPC; the remaining candidate is omniORB/CORBA plus FileMaker framing
  (README F1-F3, see [network-protocol.md](network-protocol.md)).
- `FMLayoutPB` is required for OpenMaker: layout definitions are stored as
  serialized `FMLayoutPB` messages inside the database's storage tree, and
  were decoded directly from plaintext pages of bundled `.fmp12` templates
  (observation). When a hosted file is opened, the client reads the same
  storage records from the host, so these bytes very likely travel inside the
  port 5003 protocol as opaque record values (inference, medium).
- `eap.v2` is only needed if OpenMaker supports Claris Studio data sources.
  Each data-source operation picks REST/JSON or gRPC from a flag that is
  `!Debug::HasBit(0x70)` (observation), so release builds most likely use
  REST for normal data access (inference, medium-high); "upload table to
  Claris Studio" migration calls gRPC unconditionally (observation).

## How to rerun

Scripts live in `tools/`; the binaries are only read as bytes.

```sh
# 1. Scan every Mach-O and PE file, extract descriptors, regenerate .proto
uv run --isolated --with protobuf python -I tools/protobuf_extract.py \
    --out re/work/protobuf/scan --proto-out docs/re/protobuf \
    --scan-dir "re/extracted/mac/FileMaker Pro 26/FileMaker Pro.app" \
    --scan-dir "re/extracted/win/msi/PFiles64/FileMaker/FileMaker Pro"

# 2. Recompile the regenerated files with protoc and compare to the blobs
uv run --isolated --with protobuf --with grpcio-tools python -I tools/protobuf_verify.py \
    --proto-dir docs/re/protobuf \
    --blob-dir re/work/protobuf/scan/blobs/DBEngine.x86_64 \
    --blob-dir re/work/protobuf/scan/blobs/FMEngine.x86_64
```

Outputs in `re/work/protobuf/scan/`: `blobs/<binary>.<arch>/<name>.pb` (raw
serialized `FileDescriptorProto`), `manifest.json` (offset, length, sha256,
package, deps per blob), `variants.json` (distinct byte variants per proto
name across binaries).

Extraction method (`tools/protobuf_extract.py`):

1. Split fat Mach-O into slices; PE and thin files are one slice.
2. Every `.proto` string end is a candidate. Walk back for `0x0a <varint len>`
   whose length ends exactly there (field 1 `name`).
3. From each start, walk top-level wire fields while they are legal
   `FileDescriptorProto` fields with the right wire type, in non-decreasing
   field order, singular fields at most once. Each field boundary is a
   possible end.
4. Take the longest prefix that parses with `descriptor_pb2` and has no
   unknown fields outside `*Options` messages.
5. Render `.proto` text, resolving type names relative to scope like protoc,
   printing maps, oneofs, proto3 `optional`, defaults, extensions, reserved
   ranges, custom options (decoded through a pool of all extracted files) and
   `rpc ... {}` bodies when the descriptor carries empty `MethodOptions`.

C++ `descriptor_table_protodef_*` arrays and upb `*_upbdefs.c` `descriptor[]`
arrays both hold plain serialized `FileDescriptorProto` bytes, so one scanner
covers both runtimes. Every candidate in DBEngine, FMEngine, `DBEngine.dll`
and `FMEngine.dll` parsed (zero rejected candidates). No C# base64
descriptors, protobuf.js, or jspb bundles exist outside DBEngine (searched all
extracted files for `Google.Protobuf`, `protobufjs`, `jspb.Message`,
`FileDescriptorProto`).

Verification result: all 11 regenerated files compile with protoc and, after
dropping the `json_name` that protoc adds only to `--descriptor_set_out`
(generated code omits it), are byte-identical to the embedded blobs.

Helper scripts used for the usage analysis (not needed to regenerate):
`tools/macho_callers.py` (callers of a symbol, resolving `__stubs`),
`tools/macho_func_strings.py` (strings and callees of one function),
`tools/fmlayoutpb_probe.py` (decode `LayoutObject` records from `.fmp12`).

## Inventory

103 Mach-O/PE files were scanned (24 Mac, 79 Windows). Only these contain
descriptors:

| Binary | Descriptors | FileMaker-specific | Third-party |
|---|---|---|---|
| Mac `DBEngine.framework/.../DBEngine` (per arch, x86_64 and arm64) | 111 | 7 (`eap.v2`) | 97 names + 7 duplicate `google/protobuf/*` |
| Mac `MacOS/fmxdbc_listener` (per arch) | 79 | 7 (`eap.v2`) | 69 names + 3 duplicates |
| Win `DBEngine.dll` | 79 | 7 (`eap.v2`) | 69 names + 3 duplicates |
| Mac `FMEngine.framework/.../FMEngine` (per arch) | 4 | 4 (`FMLayoutPB`) | 0 |
| Win `FMEngine.dll` | 5 | 4 (`FMLayoutPB`) | 1 (`google/protobuf/descriptor.proto`) |

No descriptors in: `FileMaker Pro` / `FileMaker Pro.exe`, Support, ClientUI,
WindowsUI, ViewSystem/libViewSystem, FMWrapper, Interop, omniORB, Skia,
PDFium, ICU, OpenSSL, or any other shipped DLL/dylib.

Descriptor locations (x86_64 slice offsets equal offsets in
`re/work/thin/<name>`; the symbol at each offset is
`descriptor_table_protodef_<file>_2eproto`):

| File | Bytes | sha256 (prefix) | Mac x86_64 slice | Mac arm64 slice | Windows file offset |
|---|---|---|---|---|---|
| `FMLayout.proto` | 770 | `36595d83fffc8013` | FMEngine `0x8a6e10` | `0x86d6dc` | `FMEngine.dll` `0xa4af20` |
| `LayoutObject.proto` | 3024 | `28631d1c247ac9e7` | FMEngine `0x89e460` | `0x864d44` | `0xa4c6f0` |
| `Style.proto` | 2699 | `8a8dca2a2734382f` | FMEngine `0x89d1f0` | `0x863a84` | `0xa4e870` |
| `Theme.proto` | 586 | `1c42e417e00700a1` | FMEngine `0x89cac0` | `0x863350` | `0xa4ff60` |
| `service.proto` | 2371 | `dfd29a890fae1ac4` | DBEngine `0x23d1660` | `0x2171b30` | `DBEngine.dll` `0xd85120` |
| `common.proto` | 941 | `25228b2665bda12a` | DBEngine `0x23d5c00` | `0x21761d0` | `0xd83960` |
| `entity.proto` | 280 | `ab53760a013a4016` | DBEngine `0x23d8780` | `0x2178c5c` | `0xd83e60` |
| `entitydef.proto` | 1597 | `f06b9bc444672847` | DBEngine `0x23d27d0` | `0x2172b54` | `0xd84000` |
| `events.proto` | 243 | `050e67e986e933ba` | DBEngine `0x23d9990` | `0x2179d34` | `0xd84980` |
| `file.proto` | 236 | `59267a45d3c0a07e` | DBEngine `0x23d0f50` | `0x217142c` | `0xd84aa0` |
| `query.proto` | 857 | `c0c0eac747eecdff` | DBEngine `0x23d7910` | `0x2177dc8` | `0xd84cc0` |

Mac `fmxdbc_listener` (xDBC listener executable) carries the same seven
`eap.v2` blobs (x86_64 slice `0xe5dbd0`-`0xe67bb0`) because it statically links
the DBEngine code; it contains `eap.platform.claris.com` and
`DAL_gRPC::connect` strings and no `FMLayoutPB`. Windows
`fmxdbc_listener.exe` (927 KB) imports `DBEngine.dll` instead and has no
descriptors of its own.

## Runtime

Observations:

- Both FileMaker packages use **C++ libprotobuf generated code**, not upb.
  Evidence: symbols `descriptor_table_protodef_service_2eproto` etc. at the
  blob offsets; data symbols `_descriptor_table_<file>_2eproto` (DBEngine
  `0x257f1a8`-`0x257fc78`, FMEngine `0x902708`-`0x903238`); C++ classes
  `eap::v2::*` and `FMLayoutPB::*` with `_InternalParse(const char*,
  ParseContext*)`, `Arena::CreateMaybeMessage<>`; RTTI strings
  `N3eap2v213CreateRequestE`; build paths
  `Source/Draco/DBEngine/ProtoBuf/service.pb.cc` and
  `Source/Draco/FMLayout/ProtoBuf/LayoutObject.pb.cc`. No `eap_v2_*_msginit`
  or `FMLayoutPB_*_msginit` upb tables exist.
- upb is present only for gRPC core internals: 505 `*_msginit` symbols
  (`envoy_*` 361, `google_*` 75, `grpc_*` 27, `validate_*` 23, `udpa_*` 10,
  `xds_*` 9). That is why DBEngine
  holds two copies of the well-known types, e.g. `google/protobuf/descriptor.proto`
  as upb `_descriptor` at `0x2255a90` (7601 bytes) and as C++
  `descriptor_table_protodef_google_2fprotobuf_2fdescriptor_2eproto` at
  `0x23e5480` (6028 bytes).
- gRPC version **1.38.0**: `grpc_version_string` (DBEngine `0x210150`)
  returns `"16.0.0"` (core version of 1.38), `grpc_g_stands_for`
  (`0x210160`) returns `"guadalupe_river_park_conservancy"`; Windows
  `DBEngine.dll` contains the string `1.38.0` and include paths
  `...\ports\grpc\gRPC-1.38.0\include\grpcpp\...`. Abseil `lts_20210324`
  symbols match.
- libprotobuf C++ runtime is built from the gRPC prebuilt tree
  (`Source/ThirdParty/Prebuilt/OSX/Release/gRPC/working/include/google/protobuf/...`).
  On Mac, DBEngine exports it (5,912 `google::protobuf` exports) and FMEngine
  imports it from DBEngine (`llvm-nm -m`: `ParseFromArray ... (from
  DBEngine)`). On Windows, `FMEngine.dll` links its own copy: it carries
  libprotobuf's own `google/protobuf/descriptor.proto` blob and its import
  table has no protobuf symbols (the only `protobuf` names in it are its own
  exports `Draco::LayoutObject::CloneUnknowns/RestoreUnknowns/SaveUnknowns`).

Inference: libprotobuf is a 3.x release from the `ParseContext` /
`InternalMetadata::Container` / `StringPiece` API era (about 3.14 to 3.19),
most likely the 3.15.x pinned by gRPC 1.38. No version string was found;
this does not affect the schemas.

DBEngine also statically contains libprotoc pieces (C++/JS generator strings
such as `jspb.BinaryWriter`, `goog.exportSymbol`, `SCCAnalyzer`) and gRPC
server reflection (`ProtoServerReflectionPlugin`, `/grpc.reflection.v1alpha.
ServerReflection/ServerReflectionInfo`), health (`/grpc.health.v1.Health/Check`)
and channelz method paths. No FileMaker code calls them (see next section);
they are dragged in by the gRPC++ build.

## Package `eap.v2` (Entity Access Pipeline)

Files: [service.proto](protobuf/service.proto),
[common.proto](protobuf/common.proto), [entity.proto](protobuf/entity.proto),
[entitydef.proto](protobuf/entitydef.proto), [query.proto](protobuf/query.proto),
[events.proto](protobuf/events.proto), [file.proto](protobuf/file.proto).
All `syntax = "proto3"`, package `eap.v2`. File option `go_package` is
`github.pie.apple.com/claris-ng/ng-data-eap/pkg/v2/rpc` (`common.proto` says
`.../pkg/rpc`), i.e. the server side is a Go service in Apple/Claris's
internal "claris-ng" org. Imports are flat (`import "common.proto"`), and
`common.proto` imports `google/protobuf/timestamp.proto`.

### Service

```
service EntityAccessPipeline {
  rpc Create(CreateRequest) returns (CreateResponse) {}
  rpc Update(UpdateRequest) returns (UpdateResponse) {}
  rpc Get(GetRequest) returns (stream GetResponse) {}
  rpc Delete(DeleteRequest) returns (DeleteResponse) {}
  rpc Events(EventFilter) returns (stream Event) {}
  rpc GetFile(GetFileRequest) returns (stream GetFileResponse) {}
}
```

Method paths (DBEngine `0x224321b`-`0x22432cc`; Win `DBEngine.dll`
`0xde7510`-`0xde7778`): `/eap.v2.EntityAccessPipeline/Create`, `/Update`,
`/Get`, `/Delete`, `/Events`, `/GetFile`. Get, Events and GetFile are
server-streaming; no client streaming.

### Key messages

- `RequestID {org_id, user_id}` leads every request.
- `CreateRequest`/`UpdateRequest` carry a oneof body: an `EntityDef`
  (table schema), an `Entity` (record), or for Update an `UpdateFields`
  (`set`/`append`/`remove` as `bytes`, encoding unknown).
- `GetRequest`/`DeleteRequest` carry a `Query`: `DataType type`
  (`ENTITY_DEF`, `ENTITY`, `FILE`), `projection` field names, a `QueryExpr`
  tree (`ConditionExpr {field, ComparisonOp, BasicValue}` and
  `LogicalExpr {AND|OR|NOT|NOR, queries}`), and `EntityQueryParams {def_id}`.
- Responses carry `ResponseStatus` (`SUCCESS`/`FAILURE`) plus a oneof of
  `Error {Code, msg}` or the payload (`int64 id` for Create).
- `Entity {id, def_id, repeated EntityField {name, BasicValue}, RevisionInfo}`.
- `EntityDef {id, name, repeated FieldDef, RevisionInfo, MetadataDef}`;
  `MetadataDef.source` is `DEFAULT` or `PRO` (table migrated from FileMaker
  Pro; matches `DAL_gRPC::getTableIsMigratedFromPro`).
- `FieldDef` has a type oneof (`Number`, `Text {max_length}`, `Bool`,
  `Timestamp {DATE|TIME|TIMESTAMP, hasTimezone}`, `List` (choice lists),
  `Binary {BYTES|URI}`), `subtype`, `display_label`, `format` map, and an
  optional `Relation {ONE_ONE|MANY_ONE|MANY_MANY, entity_def_id, field_name}`.
- `BasicValue` oneof: `int64`, `double`, `string`, `bool`,
  `google.protobuf.Timestamp`, `bytes`, `Array`, `TimestampVal {time, zone,
  offset}` (field 7 unused).
- `EventFilter {def_id}` and `Event {ADDED|CHANGED|DELETED, def_id, ids}` for
  change notifications.
- `File {id, name, description, media_type, content_url, RevisionInfo}`.

### Client or server

Observation: both a client `Stub` and a server `Service` are generated
(vtables `Stub` `0x255ecf8`, `Service` `0x255edb0`; RTTI for
`RpcMethodHandler`/`ServerStreamingHandler` instantiations). Only the client
is used:

- The sole caller of `grpc::CreateChannel` in DBEngine is
  `Draco::DAL_gRPC::DAL_gRPC(DBFileUser const*, unistring const&,
  DBAuthenticationInfo const&, HBSerialKey<5> const&)` (`0x1cd2952`), which
  then calls `EntityAccessPipeline::NewStub`.
- No FileMaker function calls `grpc::ServerBuilder::BuildAndStart`,
  `RegisterService` or `AddListeningPort`, and no FileMaker class derives from
  `EntityAccessPipeline::Service`; its six handler methods are the generated
  "unimplemented" defaults.

Conclusion: client stubs only, used by DBEngine (and the statically linked
copy in Mac `fmxdbc_listener`). High confidence.

### Who uses it and for what

`Draco::DAL_gRPC` is a DAL ("data access layer") backend created by
`Draco::DAL::CREATE_DAL(...)` for the Claris Studio data source, which is
implemented by `Draco::DBMagnoliaSource` ("Magnolia" is the internal name;
UI strings "Claris Studio Data Source", "Not signed in to Claris Studio",
`Draco::UniversalPath::kType_ClarisStudio`). RPC usage, from direct calls:

| FileMaker function | RPC |
|---|---|
| `DAL_gRPC::connect` (`0x1cd97c4`), `getTableList`, `getFieldInfo`, `getRidList`, `getRecordCount` (`0x1cd7408`), `performQuery`, `loadRecordFromHost` | `Get` (streaming read of `GetResponse`) |
| `DAL_gRPC::makeNewRowID` | `Create` with an `Entity` body |
| `DAL_gRPC::sendRecordToHost`, `Call_gRPC_Update` (`0x1cf6cb6`) | `Update` with an `Entity` body |
| `DAL_gRPC::deleteRecord`, `Call_gRPC_Delete`, `iStudioTableMover::DeleteTableOnHost` | `Delete` |
| `iStudioTableMover::CreateStudioTableSchema` | `Create` with an `EntityDef` (via `Call_gRPC_Create` `0x1cf6bea`) |
| `DBMagMig_SendSingleCreateRecord` (from `iStudioTableMover::UploadRecordsToStudioTable`) | `Create` with an `Entity` |
| `DAL_gRPC::ReceiveNotifications` (async thread lambda) | `Events` |
| none found | `GetFile` |

Connection setup in the constructor (observations from strings and callees
of `0x1cd2952`, confirmed in REA evidence
`ev_007a16762e73445fd4bb7ed04e41fa58f8bf910d2f1c5324dc48442b097f3896`):

- Target host: debug attribute `EAPServer` (`ReadDebugAttribute`, string
  `0x2242d1f`) else default `eap.platform.claris.com` (`0x2242d29`). Org name
  and the OCP API URL come from `Draco::OCPTokenSupport::GetOrgName` and
  `GetOcpApiUrl` (Claris ID / Claris Cloud token support).
- Transport security: `grpc::SslCredentials` with `pem_root_certs` read from
  the CA file returned by `FMSSLContext::GetCAFile` / `FMSHttpContext::GetCAFile`;
  `InsecureChannelCredentials` only behind the debug attribute `Insecure`.
- Call credentials: a local `JWTAuthenticator` `MetadataCredentialsPlugin`
  adds metadata key `authorization` (string `0x20e8df8`); nearby strings
  `{"alg":"HS256","typ":"JWT"}`, `{"token":`, `bearer ` and
  `Draco::TokenToJWT(unistring const&)` indicate an HS256 JWT wrapping the
  Claris ID token, sent as `authorization: bearer <jwt>` (inference, medium).
- Each call gets a deadline (`setupContext` `0x1cd5274`,
  `system_clock::now` + `Timepoint2Timespec`).

REST is the default for the data-source path:

- `DAL_gRPC::UpdateUseRESTorEAP` (`0x1cd351a`) sets the byte at `this+0x50`
  to `!Debug::HasBit(0x70)`; the constructor initializes the same byte the
  same way.
- Operations test that byte first; e.g. `getRecordCount` at `0x1cd743d`
  (`cmpb $0x1, 0x50(%rsi)`) calls `GetRecordCount_REST` when set, else builds
  an `eap.v2.GetRequest`.
- The REST twins (`GetRidList_REST`, `GetQueryRidList_REST`,
  `CreateRecord_REST`, `DeleteRecord_REST`, `GetHealthCheck_REST`,
  `BatchLoadRecordsFromHost_REST`, `MagnoliaQueryBuilder_REST`) use
  `Draco::WebServiceRequestor` with JSON and paths built from `/org/`,
  `/entity?limit=`, `&page=`, `/entity/`, `/record/`, `/record?query=` (a
  base64 JSON query, `GetAsBase64Query`), `/field`, `/status`.
- `Call_gRPC_Create` (`0x1cf6bea`), `Call_gRPC_Update` (`0x1cf6cb6`) and
  `Call_gRPC_Delete` (`0x1cf6d82`) only set a deadline and call the stub; they
  have no REST branch. The Studio table migration code
  (`iStudioTableMover::CreateStudioTableSchema`, `UploadRecordsToStudioTable`
  via `DBMagMig_SendSingleCreateRecord`, `DeleteTableOnHost`) calls them
  directly, so migration speaks gRPC regardless of the flag. Classes
  `MagnoliaMigrator` and `MagnoliaTableCloner` (RTTI names) sit on top of
  `iStudioTableMover` (`MAKE_TableMigrator`, `MAKE_TableCloner`).

Inference (medium): in release builds the debug bit is clear, so the
interactive data-source path uses REST and the gRPC `EntityAccessPipeline` is
used for Studio table migration and when the debug switch is set.

## Package `FMLayoutPB` (layout storage)

Files: [FMLayout.proto](protobuf/FMLayout.proto),
[LayoutObject.proto](protobuf/LayoutObject.proto),
[Style.proto](protobuf/Style.proto), [Theme.proto](protobuf/Theme.proto). All
`syntax = "proto2"`, package `FMLayoutPB`, no options, no services, no enums
(all enumerations are bare `int32`/`uint32`). Import graph: `Theme` and
`LayoutObject` import `Style`; `FMLayout` imports `LayoutObject` and `Theme`.

### Key messages

- `Style.proto`: value types `ColorValue` (float RGBA), `LengthValue {value,
  unit, autoValue}`, `Coord`, `Gradient`, `Image`, `BoxShadow`, `TextShadow`,
  `TextOutline`, `ParagraphTabInfo`, border-image and background helpers.
  `Property {propertyId, inherited, <one typed value field>}`;
  `PropertySet {repeated Property}`; `ComponentStatePropSet {componentId,
  stateId, propset}`; `StyleSet {repeated ComponentStatePropSet}`. The
  property model is CSS-like (per component and state, e.g. normal/hover).
- `Theme.proto`: `ThemeStyles {typeStyleSets, customStyleSets,
  deletedCustomStyleSets}` mapping object type ids and named custom styles to
  `StyleSet`; `ThemeMetadata` of name/value pairs.
- `LayoutObject.proto`: `LayoutObject {type, key, flags, bounds Rect,
  ObjectStyle style, childkey[], one type-specific sub-message, rotation=50,
  labelKey=51, version=52}`; sub-messages `TextLayoutObject`,
  `FieldLayoutObject` (with `FieldSpec {tableKey, fieldKey, repetition}`),
  `ShapeLayoutObject`, `GraphicLayoutObject`, `ExternalLayoutObject`,
  `GroupLayoutObject`, `TabLayoutObject`, `TabPanelLayoutObject`,
  `PortalLayoutObject`, `ButtonLayoutObject`, `PopoverButtonLayoutObject`,
  `PopoverLayoutObject`, `DotLayoutObject`/`DotPanelLayoutObject` (slide
  control), `SegmentedBarLayoutObject` (button bar). `LayoutObjectStore
  {rootChildKey[], objectDef[]}`. `CFCondition {flags, style,
  predefinedType}` (conditional formatting). `Rect` is
  `{top, left, bottom, right}` doubles.
- `FMLayout.proto`: `LayoutDef` (flags, table occurrence key, menu set key,
  local styles, background/box/table-grid styles, widths, print columns,
  margins, layout type, orientation, client type) and `PartDef` (part type,
  flags, page break, offset, size, break field key, styles).

Keys (`tableKey`, `fieldKey`, `valueListKey`, `libraryKey`, ...) are
`uint64` references to other catalog records (`HBSerialKey` /
`HBAbstractKey::SetSerialKeyValue`).

### Who uses it and for what

All parse/serialize calls into libprotobuf from FMEngine come from these
`Draco` load/save functions (from `tools/macho_callers.py`; addresses are
FMEngine x86_64):

| Message | Read | Write | Higher-level callers |
|---|---|---|---|
| `LayoutDef` | `FMLayout::LayoutDef::Read(HBPosition&, HBAbstractKey const&)` `0x2c979a` | `...::Write` `0x2c9c44` | `FMLayout::GetLayoutDef`, `PutLayoutDef`, `FMLayoutCatalog::GetLayoutKey/GetLayoutIndex`, `M_LayoutIndexList::Update` |
| `PartDef` | `LayoutPart::Definition::Read` `0x2da2e4` | `...::Write` `0x2da672` | `LayoutPartList::Load/Save/Recover` |
| `LayoutObjectStore` | `LayoutObjectStore::Load` `0x3ae2c0` | `...::Save` `0x3ae9c4` | `FMLayout::LoadLayout`, `FMLayout::SaveLayout`, `LayoutObjectStore::Recover`, `GraphLayoutObjectStore::Load/Save` |
| `ThemeStyles` | `ThemeDef::Load` `0x3f2372` | `ThemeDef::Save` `0x3f2e70` | `FMTheme::LoadStylesCache/WriteStyles`, `FMLayout::LoadLocalStylesCache/WriteLocalStyles` |
| `ThemeMetadata` | `ThemeMetadata::Load` `0x44daf0` | `...::Save` `0x44de60` | `FMTheme/FMLayout::LoadMetadataCache/WriteMetadata` |
| `CFCondition` | `CFCondition::Load` `0x2e7b78`, `RuntimeCFCondition::Load` `0x2e7914` | `CFCondition::Save` `0x2e7dee` | `LayoutObject::GetCFConditions/SetCFConditions/GetRuntimeCFConditions` |
| `LayoutObject` | `LayoutObject::Read(SwapIStream&)` `0x394b48` | `LayoutObject::Write(SwapOStream&)` `0x394cca` | virtual; callers not yet identified |
| `FieldLayoutObject` | `FieldLayoutObject::ReadExtended(SwapIStream&)` `0x3d4986` | `WriteExtended` `0x3d4d2a` | virtual; not yet identified |

Storage framing (observations):

- The `HBPosition`/`HBAbstractKey` variants open
  `HBCharIStream(HBPosition const&, HBAbstractKey const&)` on one value in the
  database storage tree and call `MessageLite::ParseFromIstream`; writes use
  `SerializeToOstream`. The whole stored value is one message, with no
  length prefix.
- The `SwapIStream` variant reads a 4-byte length, byte-swaps it (big-endian
  on the stream), reads that many bytes, `ParseFromArray`, then checks
  `IsInitialized`; length 0 means no message (REA evidence
  `ev_6a2d85cb8232727b9005cd2100e4ce2a5fa5dd6c70a399661e51640098759157`).
- Each `Draco` class maps to and from the message with
  `Serialize(FMLayoutPB::X*) const` / `Deserialize(FMLayoutPB::X const&)`
  (one pair per layout object class, `ObjectStyle`, `BaseStyle`,
  `PropertyMap`, `CFCondition`, `LayoutDef`, `PartDef`). Style values go
  through 45 `FMStyle::*Property::SerializeValue` and 46 `DeserializeValue`
  implementations keyed by `Property.propertyId`.
- Forward compatibility: `Draco::LayoutObject::SaveUnknowns`/`RestoreUnknowns`/
  `CloneUnknowns` (`0x3950a4`/`0x3953ae`/`0x392b54`) and
  `PropertyMap::SaveUnknowns/RestoreUnknowns` keep unrecognized fields and
  write them back. OpenMaker must preserve unknown fields when it rewrites
  layouts.

Direct confirmation from data (observation): `tools/fmlayoutpb_probe.py`
found 426 `LayoutObject` records wrapped as `LayoutObjectStore.objectDef`
(`0x12 <len>`) in plaintext pages of the six Windows
`DynamicTemplates/*/en.fmp12` files. Example from
`DynamicTemplates/Contacts/en.fmp12` offset `0x3d527`:
`type: 1 key: 9 bounds {top: 112 left: 366 bottom: 156 right: 632}
style {objectTypeId: 61 ...} fieldObj {fieldSpec {tableKey: 1065090
fieldKey: 2 repetition: 1} numOfReps: 1 keyboardType: 1 scrollbarStyle: 1}`.
Observed `LayoutObject.type` to sub-message mapping (one-to-one in this
corpus):

| `type` | Sub-message | `style.objectTypeId` seen |
|---|---|---|
| 1 | `fieldObj` | 61, 62, 63, 67 |
| 2 | `textObj` | 70 |
| 4 | `shapeObj` | 50 |
| 5 | `shapeObj` | 51 |
| 9 | `portalObj` | 83 |
| 10 | `buttonObj` | 79, 80 |
| 11 | `tabObj` | 81 |
| 12 | `tabPanelObj` | 82 |
| 13 | `externalObj` | 86 |
| 18 | `segmentedBarObj` | 78 |

Purpose: persistent, versioned encoding of layout definitions, layout parts,
layout object trees, layout-local and theme styles, theme metadata and
conditional formatting inside `.fmp12` (high confidence). Theme files shipped
in `Themes/<name>/` are CSS plus XML manifests, not protobuf; they are
converted into `ThemeStyles` when a theme is imported into a file
(inference).

Relation to port 5003 (inference, medium): the load functions read through
the generic storage layer, not through a layout-specific network API, and no
other binary references `FMLayoutPB` (FileMakerPro, Support, libViewSystem
and FMWrapper contain neither `FMLayoutPB` nor `google::protobuf` symbols).
For hosted files the same stored values are therefore expected to arrive via
the generic record/page transfer of the port 5003 protocol, as opaque
protobuf bytes.

## Mac vs Windows

- All 11 FileMaker descriptors are byte-identical across Mac x86_64, Mac
  arm64 and Windows x64 (one sha256 per file, see table above). The Mac
  `fmxdbc_listener` copies are identical too.
- All third-party descriptors present on both platforms are byte-identical.
- FileMaker Pro 26.1.1 (Mac x86_64 `FMEngine` and `DBEngine`) embeds the
  same 11 FileMaker descriptors, byte for byte (same sha256s; rerun of
  `tools/protobuf_extract.py`, 2026-11-19): 26.1.1 changed no `FMLayoutPB`
  message, and its new layout options are bits of existing fields
  (layout-model.md 5.3.2, 5.3.4, 5.4.1).
- Mac `DBEngine` carries 28 more third-party files than Windows
  `DBEngine.dll`: `envoy/admin/v3/config_dump`, `envoy/annotations/resource`,
  `envoy/config/bootstrap/v3/bootstrap`, `envoy/config/metrics/v3/stats`,
  `envoy/config/overload/v3/overload`,
  `envoy/extensions/transport_sockets/tls/v3/cert`, envoy discovery services
  (`cds`, `ads`, `eds`, `lds`, `rds`, `srds`, `csds`), `envoy/type/matcher/v3/
  {node,path,struct}`, `google/api/{annotations,http}`,
  `google/protobuf/{api,compiler/plugin,field_mask,source_context,type}`,
  `src/proto/grpc/channelz/channelz`, `src/proto/grpc/reflection/v1alpha/
  reflection`, `udpa/type/v1/typed_struct`, `xds/core/v3/{resource,
  resource_name}`. The Mac `fmxdbc_listener` executable has exactly the
  Windows set (69), so the difference is linker dead-stripping of a dylib
  that exports everything, not a functional difference (inference, high).
- Windows `FMEngine.dll` links its own libprotobuf; Mac FMEngine imports it
  from DBEngine.
- No version skew between platforms.

## Third-party descriptors (names only)

Union across all binaries, 97 files: `envoy/admin` (1), `envoy/annotations`
(2), `envoy/config` (32), `envoy/extensions` (9), `envoy/service` (9),
`envoy/type` (14), `google/api` (2), `google/protobuf` (12: any, api,
compiler/plugin, descriptor, duration, empty, field_mask, source_context,
struct, timestamp, type, wrappers), `google/rpc/status`, `src/proto/grpc`
(channelz, reflection v1alpha), `udpa/annotations` (5), `udpa/type/v1/
typed_struct`, `validate/validate`, `xds/core/v3` (6). These are gRPC 1.38's
xDS/upb and libprotobuf well-known types; none are used by FileMaker code
beyond `google/protobuf/timestamp.proto` (imported by `eap.v2` `common.proto`).
The full list per binary is in `re/work/protobuf/scan/manifest.json`.

## REA evidence

| Evidence ID | Subject |
|---|---|
| `ev_6a2d85cb8232727b9005cd2100e4ce2a5fa5dd6c70a399661e51640098759157` | Ghidra decompile, FMEngine `Draco::LayoutObject::Read(SwapIStream&)` `0x394b48` (BE uint32 length, `ParseFromArray`, `IsInitialized`) |
| `ev_007a16762e73445fd4bb7ed04e41fa58f8bf910d2f1c5324dc48442b097f3896` | Ghidra decompile, DBEngine `Draco::DAL_gRPC::DAL_gRPC(...)` `0x1cd2952` (sets the REST flag at `this+0x50` from a `Debug::HasBit` test, reads debug attributes `EAPServer` and `Insecure`, `OCPTokenSupport::GetOcpApiUrl`, `SslCredentials` with `pem_root_certs` or `InsecureChannelCredentials`, `TokenToJWT` + `MetadataCredentialsFromPlugin`, `CreateChannel`, `EntityAccessPipeline::NewStub`) |

Other claims cite addresses in the x86_64 slices; they were obtained with
`llvm-objdump` and `llvm-nm` and can be rechecked with `rea decompile
--procedure <address>` against the pre-analyzed projects.

## Open questions

1. `LayoutObject::Read/Write(SwapIStream&)` and
   `FieldLayoutObject::ReadExtended/WriteExtended` are reached only through
   vtables. Which features use this length-prefixed stream form (clipboard,
   undo, object transfer, server-side layout sync)? Needs Ghidra xrefs on the
   vtable slots (`LayoutObject` vtable `+0x148` is called right after parse).
2. Enumerations carried as integers: `LayoutObject.type`, `flags` bit
   meanings, `Property.propertyId` table (the 45 `FMStyle::*Property`
   classes), `ComponentStatePropSet.componentId/stateId`,
   `LengthValue.unit`, `PartDef.type`, `LayoutDef.layoutType/
   layoutClientType`. These belong to [layout-model.md](layout-model.md);
   `LayoutObjectFactory::Create(unsigned char const&)` (`0x3dceee`) and the
   property registration code are the places to read.
3. Which HB storage paths (`HBAbstractKey` values) hold each message inside
   `.fmp12`, and whether hosted-file transfer on port 5003 carries them
   unchanged (inference above). Confirm with a capture once the port 5003
   framing and TLS are understood.
4. `eap.v2.UpdateFields.set/append/remove` are `bytes`; the encoding is not
   visible from the schema and `UpdateFields` was not seen being built by
   FileMaker code.
5. Is the gRPC data-source path reachable in release builds other than
   through `Debug::HasBit(0x70)`? Is the Studio migration feature exposed in
   26.0.3 UI? Only matters if OpenMaker supports Claris Studio.
6. Exact libprotobuf version (no version string found; does not affect the
   schemas).

## Suggested next steps

- Layout workstream: build the `type`, `propertyId`, `componentId`/`stateId`
  tables from FMEngine (`LayoutObjectFactory::Create`, `FMStyle` property
  registration) and validate with `tools/fmlayoutpb_probe.py` over the 66
  bundled `.fmp12` templates per platform (`DynamicTemplates/<name>/<lang>.fmp12`;
  only the six `en` files were probed so far).
- File-format workstream: locate the storage keys under which `LayoutDef`,
  `PartDef`, `LayoutObjectStore`, `ThemeStyles`, `ThemeMetadata` and
  `CFCondition` live; the templates give known-good samples.
- Network workstream: treat gRPC/protobuf as out of scope for port 5003; look
  for `FMLayoutPB` bytes inside decrypted 5003 traffic as a cross-check.
