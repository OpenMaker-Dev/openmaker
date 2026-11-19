# FileMaker Pro 26 client: architecture map and catalogs

Workstream: architecture map, binary inventory, third-party components,
interoperability catalogs, client vs server responsibilities.

Reference build: FileMaker Pro 26.0.3.307 (Mac universal, x86_64 slice) and
26.0.3.307a (Windows x64). Evidence conventions: symbol names come from the
x86_64 export tables demangled with `llvm-cxxfilt -_`; addresses are x86_64
virtual addresses in the thinned slices under `re/work/thin/` (SHA-256 in
`re/work/thin/SHA256SUMS`); "obs" = observation, "inf" = inference,
"unk" = unknown.

Note for other workers: the files in `re/work/symbols/*.x86_64.txt` still
contain mangled names (Mach-O symbols carry an extra leading underscore, so
plain `llvm-cxxfilt` leaves them untouched). Demangled copies are in
`re/work/arch/dsym/<name>.txt` (`llvm-cxxfilt-18 -_`).

## 1. Summary

The internal engine namespace is `Draco` (obs: about 27k `Draco::` symbols
across DBEngine, FMEngine, Support). The product is a stack of five layers:

```
+--------------------------------------------------------------------------+
| App shell + UI         Mac: FileMaker Pro (ObjC/Swift/C++)               |
|                        Win: FileMaker Pro.exe (MFC + Codejock),          |
|                             ClientUI.dll, Interop.dll (C++/CLI),         |
|                             WindowsUI.dll (WPF), FMRSRC.dll (resources)  |
|   charts (ChartDirector), PDF writer, spelling, web viewer, licensing,   |
|   Manage Database/relationship graph, script workspace, inspector        |
+--------------------------------------------------------------------------+
| View system            libViewSystem.dylib / ViewSystem.dll              |
|   IView/BaseView tree, IGraphics backends: CoreGraphics (Mac),           |
|   Skia + GDI+ (Win), event system, print operations                      |
+--------------------------------------------------------------------------+
| FMEngine               layout model, styles/themes (CSS), script engine, |
|                        import/export (text, Excel, XML, ODBC), XML       |
|                        formats, object transfer (copy/paste), add-ons,   |
|                        email (libetpan), Data API engine (FMWIP)         |
+--------------------------------------------------------------------------+
| DBEngine               storage (HBAM pages, B-trees, cache, log, EAR),   |
|                        schema catalogs, records/finds/sorts/summaries,   |
|                        calc engine (all functions), SQL engine (FQL),    |
|                        client-server (omniORB CORBA RCFMP12), accounts,  |
|                        ESS/ODBC, plug-in host, Insert from URL (curl),   |
|                        AI (LLM providers, embeddings, semantic find),    |
|                        Claris Studio (gRPC eap.v2) and Claris ID/cloud   |
+--------------------------------------------------------------------------+
| Support                text/unicode (ICU 76 static on Mac), locales,     |
|                        FixPt decimal numbers, DateTime, files/paths,     |
|                        threads, certificates, SRP, images/media metadata |
+--------------------------------------------------------------------------+
FMWrapper = public plug-in SDK C ABI (FM_* functions) over DBEngine/Support
```

DBEngine and FMEngine are shared with FileMaker Server and FileMaker Cloud
(inf, section 9): they contain server-only pieces (AWS SDK EC2/S3/KMS/SQS
clients, shared-memory page cache, Data API JSON engine, server script
sessions) that OpenMaker does not need.

## 2. Binary inventory

### 2.1 macOS (`FileMaker Pro.app/Contents`, universal x86_64 + arm64)

Sizes: universal file / x86_64 slice. Export counts are from the x86_64
symbol lists (`T`+`S`+`D`).

| Binary | Universal / x86_64 | Exports | Role | Links (non-system) |
|---|---|---|---|---|
| `MacOS/FileMaker Pro` | 135.9 MB / 68.4 MB | 40,814 | App shell, all UI, charts, PDF writer (PDFHummus + FreeType + libjpeg + libtiff), spelling engine, licensing, analytics, update | DBEngine, FMEngine, Support, FMWrapper, libViewSystem, libchartdir.7, omniORB set, libssl/libcrypto 3, libldap/liblber, Sparkle; system libcurl.4, libz, WebKit, AVFoundation, AuthenticationServices, Intents, Swift runtime |
| `Frameworks/DBEngine.framework` | 120.2 MB / 61.8 MB | 64,882 | Database engine (see section 1) | Support, omniORB (ORB, Dynamic, sslTP, httpTP, thread, ZIOP, ZIOPDynamic), libssl/libcrypto, libsasl2, libz, libresolv, CoreML, Vision, CoreImage, CFNetwork, Security |
| `Frameworks/FMEngine.framework` | 31.3 MB / 15.8 MB | 19,189 | Layout model, script engine, import/export, XML, email, add-ons | DBEngine, Support, libssl/libcrypto, system libxml2, libxslt, libexslt, libxar, libsasl2, libiconv, libz |
| `Frameworks/Support.framework` | 82.5 MB / 41.3 MB | 14,202 | Foundation library; 33 MB `__const` is the static ICU 76 data | libssl/libcrypto, PDFKit, Vision, CoreImage, ImageIO |
| `Frameworks/FMWrapper.framework` | 0.7 MB / 0.34 MB | 255 | Plug-in SDK C ABI (`FM_Text_*`, `FM_ExprEnv_*`, ...) | DBEngine, Support |
| `Frameworks/libViewSystem.dylib` | 2.9 MB / 1.46 MB | 2,594 | View tree, graphics abstraction, events, printing | DBEngine, Support, Cocoa, QuartzCore, CoreText |
| `MacOS/fmxdbc_listener` | 115.8 MB | n/a | xDBC (ODBC/JDBC sharing) listener; DBEngine code statically linked (inf: no DBEngine dylib dependency, size) | omniORB, libldap, CoreML, Vision, PDFKit, WebKit |
| `XPCServices/PSConversionHelper.xpc` | 0.1 MB | n/a | PostScript conversion helper (inf from name) | |
| `Frameworks/libchartdir.7.dylib` | 6.8 MB | | ChartDirector 7 | |
| `Frameworks/libomni*4.3.4.dylib` | 5.8+3.9+0.5+0.5+0.4+0.2+0.2 MB | | omniORB 4.3.4 (ORB, Dynamic, ZIOP, ZIOPDynamic, httpTP, sslTP, omnithread 4.4) | |
| `Frameworks/libcrypto.3.dylib`, `libssl.3.dylib` | 7.8 / 1.3 MB | | OpenSSL 3.5.7 | |
| `Frameworks/libldap.dylib`, `liblber.dylib` | 0.8 / 0.3 MB | | OpenLDAP 2.6.7 | system libsasl2 |
| `Frameworks/Sparkle.framework` (Versions/B) | 1.6 MB | | Sparkle 2 auto-update (`CFBundleShortVersionString` 2.2.0) | |

Bundle metadata (obs, `Contents/Info.plist`): `com.filemaker.client.pro12`,
minimum macOS 14.0, URL schemes `fmp`, `fmp26`, `fmpoauthA26` (OAuth
callback), document types `fmp12`, `fmpsl` (snapshot link), `fmplugin`,
`fmaddon`, `flb` (labels), `mpr` (spelling dictionary), `upr` (user
dictionary), `fmcert` (license certificate). Sparkle EdDSA public key in
`SUPublicEDKey`.

### 2.2 Windows (`PFiles64/FileMaker/FileMaker Pro`, x64)

| Binary | Size | Exports | Role | Notable imports |
|---|---|---|---|---|
| `FileMaker Pro.exe` | 30.6 MB | 618 | MFC app shell | ClientUI, Interop, FMEngine, DBEngine, Support, ViewSystem, ToolkitPro1720, mfc140u, pdfium, chartdir70, WinSparkle, PROOFREADER, WebView2Loader, libssl/libcrypto, zlibwapi, gdiplus, WININET |
| `DBEngine.dll` | 19.0 MB | 15,229 | Database engine | omniORB434 (ORB, Dynamic, sslTP, httpTP), omnithread44, libcurl, libssl/libcrypto, zlibwapi, ODBC32, WINHTTP, WININET, Secur32, DNSAPI, ncrypt, bcrypt |
| `FMEngine.dll` | 14.8 MB | 13,037 | Layout/script/import/export | DBEngine, Support, libxml2, libxslt, libexslt, libetpan, libssl/libcrypto, zlibwapi |
| `Support.dll` | 4.2 MB | 6,757 | Foundation library | icuuc76, icuin76, pdfium, libzbar64-0, WindowsCodecs, gdiplus, WINHTTP, libcrypto |
| `ViewSystem.dll` | 0.9 MB | 2,591 | View system (Skia and GDI+ graphics backends: `SkiaGraphics`, `GDIPlusGraphics`, `NullGraphics`) | skia, gdiplus, mfc140u |
| `ClientUI.dll` | 9.1 MB | 3,450 | Cross-platform UI models (inspector `PropBase`, accounts, import mapping, upload, server picker, license manager, add-ons); on Mac these live inside the main executable | DBEngine, FMEngine, Support, ViewSystem, WebView2Loader |
| `Interop.dll` | 1.7 MB | 1,343 | Mixed-mode C++/CLI bridge between MFC/native models and WPF (`WPFC*` wrapper classes, `*InteropWindow`) | DBEngine, FMEngine, Support, ClientUI, mfc140u, mscoree |
| `WindowsUI.dll` | 1.4 MB | managed | WPF dialogs (.NET Framework 4.8, AnyCPU IL-only, assembly version 26.0.3.307) | |
| `FMWrapper.dll` | 0.5 MB | 439 | Plug-in SDK C ABI | DBEngine, Support |
| `fmxdbc_listener.exe` | 0.9 MB | 187 | xDBC listener | DBEngine, Support |
| `ZAuth.exe` | 0.08 MB | 2 | "FileMaker Authenticator": elevated helper that writes the license key to HKLM preferences (obs: `SaveLicenseKey`, registry paths) | Support |
| `Extensions/<Lang>/FMRSRC.dll` | 4.9 MB each | | MFC resource DLL per language | |
| `Setup/*_rsrc.dll`, `Setup/Uninstall.exe` | | | Installer resources | |
| third-party DLLs | | | see section 4 | |

Windows has no ZIOP DLL (obs: only `omniORB434`, `omniDynamic434`,
`omnisslTP434`, `omnihttpTP434`, `omnithread44`).

### 2.3 Mac and Windows equivalents

| macOS | Windows |
|---|---|
| `FileMaker Pro` (shell + UI models + charts glue + PDF writer + spelling) | `FileMaker Pro.exe` + `ClientUI.dll` + `Interop.dll` + `WindowsUI.dll` + `FMRSRC.dll` + `ToolkitPro1720vc141x64U.dll` + `ProofReader.dll` |
| `DBEngine.framework` (static libcurl 8.20.0-DEV + libssh2 1.11.1, iODBC 3.52.13) | `DBEngine.dll` + `libcurl.dll` (+ `ODBC32`) |
| `FMEngine.framework` (static libetpan; system libxml2/libxslt) | `FMEngine.dll` + `Libetpan.dll` + `libxml2.dll`/`libxslt.dll`/`libexslt.dll` + `libiconv.dll` |
| `Support.framework` (static ICU 76; PDFKit; Vision for QR and Live Text) | `Support.dll` + `icu{dt,uc,in}76.dll` + `pdfium.dll` + `libzbar64-0.dll` |
| `libViewSystem.dylib` (CoreGraphics backend) | `ViewSystem.dll` + `skia.dll` (+ GDI+) |
| `FMWrapper.framework` | `FMWrapper.dll` |
| `libchartdir.7.dylib` | `chartdir70.dll` |
| `libomni*4.3.4.dylib` | `omni*434_vc17_rt.dll`, `omnithread44_vc17_rt.dll` |
| `libssl.3` / `libcrypto.3` | `libssl-3-x64.dll` / `libcrypto-3-x64.dll` |
| `libldap` / `liblber` + system SASL | `WLDAP32` (via libcurl) + `libsasl.dll` + `SASL2/*.dll` (Cyrus SASL plugins: PLAIN, LOGIN, CRAM-MD5, DIGEST-MD5, ANONYMOUS; used by libetpan) |
| `Sparkle.framework` | `WinSparkle.dll` |
| WKWebView (`PlatformWKWebViewerMac`) | WebView2 (`WebView2Loader.dll`, `Microsoft.Web.WebView2.*`) |
| `PSConversionHelper.xpc` | n/a |
| `Resources/*.lproj/FMStrs.dls`, `Base.lproj/*.nib`, `*.lproj/*.strings` | `Extensions/<Lang>/FMStrs.dls`, `FMRSRC.dll`, `<lang>/WindowsUI.resources.dll`, `XTPTrans/*.xml` |

### 2.4 .NET assemblies (Windows)

| Assembly | Facts (obs) | Role |
|---|---|---|
| `WindowsUI.dll` | IL-only, .NET Framework 4.8, 693 TypeDefs, 7,939 MethodDefs; references PresentationFramework, WebView2 Wpf/Core, Microsoft.Toolkit.Uwp.Notifications, System.Windows.Forms. REA Evidence `ev_e04b45c4b0957cdfc9ffa9b00caa3e433cc8c11b067d7875911951efcc129b5d` (artifact), `ev_20488308055dde06e47f42f801c48d7c6fc809a4ee5c9307293147d40eacd41e` (type inventory; member decoding exceeded REA capacity) | WPF dialogs. Namespaces: `WindowsUI.Scripting` (73 types, Script Workspace), `DataImport` (35, import mapping), `Addon` (16), `ButtonBar` (14), `SecurityAccess` (14), `UploadtoServer` (13), `ManageLayouts` (11), `CustomFunctions` (10), `FieldPicker`, `LayoutObjectTree`, `LicenseStatus`, `QuickOpen`, `InsertEmbeddingSummary` (AI), `AppleSignIn`, `About`, `DocumentWorkspace` |
| `Interop.dll` | Mixed-mode C++/CLI (native entry point, x64), 991 TypeDefs, 5,050 MethodDefs. REA Evidence `ev_c3e29553b35e6c4870e2850aa73ec47fec5c7f499a091bd977ad447e96bab755`, `ev_5eac9b22ee290677b2b137feb084ff67b0d3fd673b37fb728a77038df3260d17` | Hosts WPF windows in MFC (`WPFC*` controls, `*InteropWindow`, `*ManagedModel` wrappers) and marshals models from ClientUI/DBEngine |
| `<lang>/WindowsUI.resources.dll` | satellite resources | WPF localization |
| `Microsoft.Web.WebView2.{Core,Wpf}.dll` 1.0.1661.34, `Microsoft.Toolkit.Uwp.Notifications.dll` 7.1.3 | vendor | web viewer, toast notifications |

`FileMaker Pro.exe.config` pins the CLR to .NET Framework 4.8.

### 2.5 Resource payloads worth knowing

- `Extensions/<Lang>/FMRSRC.dll` (Windows): 1,937 resources (REA Evidence
  `ev_434b5a05fcfae7a60d4c5ffc8041f488b2b8e3e29b41fb3e573531ad8a719af1`):
  285 dialogs, 156 menus, 117 RT_STRING blocks (689 strings, mostly tooltips),
  308 RCDATA entries that are XAML for the WPF dialogs, 500 PNG, 44
  `DLGBINDING`, 49 `XMHP`, 19 `FILTER_LIST`, plus `TEMPLATE`, `CLUT`,
  `FILLRECTDATA`. The calc/script/error catalogs are not here (section 5).
- `FMStrs.dls` (every non-English locale, both platforms) starts with
  `00 01 00 00 00 02 00 01 00 05 00 02 00 02 C0` + `HBAM7`: the localized
  string tables are stored in the same HBAM7 container format as `.fmp12`
  files (obs). English strings are compiled into DBEngine instead. These files,
  and `DynamicTemplates/Assets/<lang>.fmp12`, are a free test corpus for the
  file-format workstream.
- `Themes/<name>/` (55 themes): `manifest*.xml` + `<name>.css` (FileMaker CSS
  dialect) + images.
- `AddonModules/<name>/` (21 add-ons): `template.xml`, `<lang>.xml`,
  `records_<lang>.xml`, `info*.json`.
- `Tiktokens/cl100k_base.tiktoken`: BPE ranks for the tokenizer used by
  `Draco::TokenModelGenerator::Cl100KBase` and `Get(LastStepTokensUsed)`,
  `GetTokenCount()`.
- `jsrunner.html` (Mac): bundles moment.js 2.29.4 and moment-timezone;
  executed by `Draco::JSRunner` (Support) in a web view for time zone work
  (inf: `DBMagnoliaTZHelper` in Support).
- `DDR/*.xsl` (Database Design Report), `fmpro.sdef` (AppleScript),
  `WebViewerTemplates.xml`, `FMDefaultFields.xml`, `DefaultGlyphs/*.svg`
  (147 button-bar glyphs), `Extensions/Dictionaries/*.mpr`,
  `Extensions/Labels/*.flb`.

## 3. Dependency graph (Mac, from `llvm-objdump --macho --dylibs-used`)

```
FileMaker Pro -> FMWrapper -> DBEngine -> Support
              -> libViewSystem -> DBEngine, Support
              -> FMEngine -> DBEngine, Support
              -> DBEngine, Support, libchartdir, omniORB*, Sparkle, libldap
DBEngine undefined-symbol imports by library (llvm-nm -u -m): Support 1142,
  libcrypto 352, libomniORB4 107, libssl 91, libomnisslTP4 27,
  libomnihttpTP4 18, libsasl2 14, libz 12, CoreML 8, libomnithread 5,
  libomniDynamic4 4, Vision 4, libomniZIOP* 0
```

## 4. Subsystem map

Symbol counts below are exported x86_64 symbols per class scope, assigned by
ordered regex rules (`tools/arch_subsystems.py`; full output in
`re/work/arch/subsystems.txt`). About 12.6k symbols stay unclassified (mostly
`Draco::` free functions and small UI helpers). Counts indicate size, not
importance.

| Subsystem | Symbols (binary) | Top classes (symbols) |
|---|---|---|
| Layout model, styles, themes | 9,088 (FMEngine 4,734; FMP 4,243) | `FMStyle::*` (246+), `Draco::BaseStyle` 203, `Draco::FMLayout` 158, `Draco::FMWindowModel` 153, `Draco::LayoutObject` 148, `FMLayoutPB::*` (protobuf layout messages), `Draco::ThemeDef`, `Draco::FMTheme`, `CSSParser`, FMP `LayoutObjectView` 211 |
| App shell & UI | 8,216 (FMP 7,864) | `FMDocWindow` 306, `PropBase` 277 (inspector), `CFMProApp` 132, `SelectionViewManager`, `DocWindowStatusModel`, `Draco::FMSession` 92, `Draco::FMFileUser` 87 |
| Claris Studio, cloud, Claris ID | 5,571 (DBEngine 5,236) | `Draco::CloudFactory`, `Draco::FMID`/`FMIDMgr`, `Draco::DBMagnoliaSource` (Claris Studio tables), `Draco::DAL_gRPC`, `eap::v2::*` (gRPC), `io::swagger::client::*` (Claris Cloud admin OpenAPI client), `Draco::FileBox` (S3 storage, server side), `Draco::FMFileTransferWebServicesClient`, `Draco::KeyStorageRequestor`, `Draco::OCPTokenSupport` |
| View system & rendering | 5,277 (FMP 3,300; libViewSystem 1,891) | `IView` 170, `BaseView` 160, `CGGraphics`, `IGraphics`, `ScrollView`, `EventSystem`, FMP `CSSBlockView` 140, `XText` 110, `XInteractiveText` 103, `TextView`, `FieldTextView` |
| SQL engine (FQL, ExecuteSQL) | 3,528 (DBEngine) | ANTLR4 grammar `FQLSQLLexer`/`FQLSQLSemanticAnalyzer` (the live parser; `FQLSQLParser` is dead code, [execute-sql.md](execute-sql.md)), runtime `libantlr4-runtime.so.4.13.1`, `FQL::FQLParserDriver`, `FQL::Query`, `FQL::Expr`, `FQL::ExecuteSQLCalc`, `FQL::ExecuteFileSQL` |
| DB engine: records, finds, sorts, summaries, relationships | 2,816 (DBEngine 2,223) | `DBBinaryData` 175 (container data), `DBData` 95, `DBField` 91, `DBTransaction` 64, `DBCursor` 64, `DBTableProxy` 63, `DBRecordCursor`, `DBTableAlias`, `DBValueList`, `DBAbstractRidList`, `DBQueryProgram`, `DBFinder`, `DBSortTree`, `SummaryCompute`, `DBRelatedSet` |
| Script engine | 2,481 (FMEngine 1,215; FMP 1,249) | `Draco::ScriptRuntimeBase` 286, `Draco::ScriptStep` 180, `Draco::ParameterListFactory` 119, `FMScriptCatalog`, `ScriptTrigger`, `TriggerState`, `SMScriptRuntime`/`SMSession` (server-side runtime), FMP `NativeScript` 125, `ScriptRuntime` 98, `ScriptDebuggerModel` |
| XML formats | 2,397 (FMEngine 2,123) | `V2_ScriptStepXML` 205, `V1_ScriptStepXML`, `XMLIScriptStep`/`XMLScriptStep`, `XMLOStreamLayout`/`XMLIStreamLayout`, `DynamicTemplateXML_Director` 217, `FM_XML_Results`, `FMPXMLParser` (fmpxmlresult), `IXMLPackageCreator` (Save a Copy as XML), `DDReport` |
| Storage engine | 2,163 (DBEngine 1,125; FMEngine 723) | `HBFile` 99, `HPage` 82, `HPosInfo` 73, `HCacheMgr` 70, `HSCacheMgr` 99 (shared-memory cache, server), `HBPosition`, `HBAbstractKey`, `HBAMSlot`, `HBEngine`, `HBLogFile`, `HBEncryption*` (EAR), `DBIndexTree`; FMEngine `fmfs_imp::*` (inf: file-system abstraction) |
| Charts | 2,104 (FMP) | `FMChart::ChartFormatDefinition` 132, `FMChart::ChartDataDefinition` 89, `ChartDirector_ChartRendererBase` 78, `FMChart::ChartModel`, `AxisModel` |
| Import/export | 1,960 (FMEngine 1,189) | `FMImportExcel`, `FMImportExcel2007`, `Excel2007FileParser`, `ExcelWriter2007`, `ooxml::*`, `FMImportText`, `FMImportXML`, `FMImportODBC`, `FMImportDBase`, `FMImportManager`, `FMExportManager`, `FMExportXML`, `FMXMLImportProfile` |
| Text, Unicode, locale | 1,905 (Support) | `Draco::Text` 105, `TextBuffer` 95, `unistring` 66, `International` 60 plus about 45 per-language `*_International` collation classes, `SCSUCompress`/`SCSUExpand` (SCSU text compression), `DotStringsManager` |
| Printing / PDF output | 1,848 (FMP 1,455) | `HummusGraphics`, `HummusImageVS`, `FMPDF::Hummus_imp::*` (Save Records as PDF, append to PDF), `DefinitionPrintJob`, `XPrintInfoCocoa`, `PrintSettingsCommon`, `PageFormatCommon`, `Draco::FMSaveAsPDFManager`, `Draco::PDFFileMerger` |
| Calculation engine | 1,693 (DBEngine 1,089) | `DBDataArg` 317 (function implementations), `Support Draco::FixPt` 118 (decimal number type), `DBCalcSolver` 81, `DBCalcScan` 58, `DBCalcParse` 45, `DBCalcFormula` 38, `DBFunction`, `DBFunctionCatalog`, `DBVarArgsOper`, `DBPolishEnv` |
| Accounts, privileges, security, EAR | 1,534 | `DBPrivileges` 56, `DBUserAccount` 50, `DBUserAccountCatalog`, `DBAuthenticationInfo` (password, NT, OAuth, Claris ID token, Sign in with Apple), `DBPrivilegeExtensions`, `HBEncryptionHandler`, `ConvertEncrypted`, Support `Certificate`, `FMCertificateStore`, `SRP_Client`/`SRP_Server`, `PasswordHash`, `PasswordKeychain` |
| Platform & OS utilities | 1,483 (Support) | `QuadChar` 194, `UniversalPath` 117, `File`, `DateTime`, `Directory`, `Thread`, `ShmSegment`, `MsgQ` |
| Client-server networking | 1,455 (DBEngine 1,134) | `DBSession` 115, `RCNetworkStack` 76, `OpenSSL` 60, `RCTransaction` 59, `RCConnection` 48, `RCNetAddress`, `NetGuestFile`, `NetHostSession`, `DBRemote*`, CORBA `_objref_RCFMP12` |
| Manage Database UI | 1,425 (FMP) | `JGE*` (relationship graph editor), `FMDefineFieldsPage`, `FMDefineRelationsPage`, `FMDefineTablesPage` |
| DB engine: schema catalogs | 1,164 (DBEngine) | `DBFileUser` 155, `DBCatalog` 73, `DBCatalogMember`, `DBFileReference`, `DBFieldCatalog`, `DBMetaTree`, `DBTableAliasCatalog`, `DBBaseDirectoryCatalog`, `DBResourceCacher` |
| ESS / ODBC / xDBC | 876 | `DBOdbcSource` (+ `MySQLHandler`, `OracleHandler`, `SQLServerHandler`, `SQLServer2000Handler`), `FMODBC::ODBCQuery`/`ODBCConnection`/`WinOdbcSsoServer`, FMEngine `FMSimpleODBC`, `FMODBCConnProfile`, FMP `xDBCSharingDlg` |
| Schema object transfer | 862 (FMEngine) | `ObjectTransferUtility` 140, `ObjectTransferLayouts` 111, `ObjectTransferFields`, ... (copy/paste of tables, fields, scripts, layouts, themes, accounts between files) |
| Media & containers | 664 | Support `Image2D`, `ID3Metadata`, `MP4Metadata`, `MPEGFrame`; FMP `ContainerData`, `InsertFromDeviceModel`, `ManageContainersModel` |
| Insert from URL / HTTP | 475 | `FMWS_HTTPHeader`, `WebServiceRequestor`, `HTTPHeader`, `OAuthServiceRequestor`, `FMURLRequest`, `FMURLParser`, `FMURLConnection`, FMEngine `InsertFromURLBinaryDataProvider` |
| Licensing | 416 (FMP) | `ProLicenseStatusModel`, `FMProLicenseMgr`, `FMLicenseServices::*`, `LicenseCode::Scheme1Verify`/`Scheme3Verify`, `LCVerifyLib` |
| Plug-in API | 390 | FMWrapper `FM_*` (252), `DBPlugIn` 58, `DBPluginFunction`, `DBPluginOpcode`, FMEngine `PluginHelper` (xar packaging) |
| Developer tools | 371 (FMP) | `FMDevTool*` (Save a Copy As, generate apps), `CWatchViewerPage`, `CDataViewerSheet` |
| Add-ons, templates, patches | 362 | `AddonManager`, `DBAddon`, `DBAddonCatalog`, `PatchGenerationManager`, `AddonTemplateMetadata` |
| Web viewer / JavaScript | 354 | FMP `PlatformWKWebViewerMac`, `ProWebViewerView`, `WebViewerSetupModel`; FMEngine `WebViewerBase`, `WebViewerManager`; Support `JSRunner` |
| Spelling | 255 (FMP) | `SpellChecker`, `SpellCheckDialog`, statically linked spell engine (`PD*`/`PR*` C functions), Windows `ProofReader.dll` |
| AI / ML | 241 | `FMLLM::FMLLMKit` 59, `FMLLM::FMLLMHelper` 17, `FMLLMProviderApple`, `FMLLMProviderGoogle`, `CoreMLModel` 18, `DecisionTree` 18, `RandomForest`, `DBSemanticFind` 16, `LLMDebugLogger`, `DBEmbeddingConverter`; Support `TokenEncoding`, `TokenModelGenerator`, `VectorMath`, `FMVisionKit` |
| Email | 151 | FMEngine `CMail`, `CSendMailBase`; FMP `SendMailModelEx`, `SMTPOptionsModel` |
| Data API engine | 146 (FMEngine) | `HLClient` 58, `FMWIP::WIPJSON` 43, `HLUtils`, `HLTransactionMgr`, `HLSession` (used by "Execute FileMaker Data API" step and by the server's Data API, inf) |
| Usage analytics | 73 (FMP) | `ProAnalyticsManager_RCp1`, `ProAnalyticsFiles_RCp1`, `ProAnalyticsLog_RCp1`, `ProAnalyticsCommunicator_RCp1` |
| Auto-update | 21 (FMP) + Sparkle | `InProductNotification*`; Sparkle 2 `SPUUpdater` property |

Third-party symbol volume (same run): AWS SDK 23,660; gRPC/protobuf/abseil/upb
15,254; ICU 8,748; ObjC metadata 3,946; libc++ 3,906; ANTLR4 3,520 (two copies:
DBEngine and FMEngine); libetpan 1,739; libssh2/OpenLDAP/iODBC/CBOR 1,565;
PDFHummus 953; JSON libraries 902; Boost 714; FreeType/libjpeg/libtiff 502.

### 4.1 Subsystem notes

**Client-server networking** (obs unless noted). CORBA interface
`IDL:RCFMP12:1.0` exposes six operations (`_objref_RCFMP12::Connect`,
`Reconnect`, `Disconnect`, `Perform`, `Notify`, `Tickle`). FileMaker
operations are serialized objects registered through `CreationFunction`
factories and carried by `Perform` (inf). The registered remote operations
in DBEngine are:

`RCAdjustReferenceCount RCBuildIndex RCChangeSessionName RCCommitRecords
RCCompareModCounts RCConnectToServer RCCreateRecord RCDBTranInfo
RCDownloadFile RCDownloadFromHost RCDownloadFromTemp RCDownloadList
RCDownloadThumbnail RCDownloadWithLock RCGetCalcGuestCount RCGetContainerURL
RCGetFileList RCGetFileSize RCHostTimeStamp RCLogin RCLogout RCOpen
RCSendPSOSRetToClient RCSerialize RCTransferContainer RCUpdateTable
RCUploadBinaryData RCUploadListToHostTempLocation RCUploadToHost
RCUploadWithLock RCVerifyContainer DBRemoteLock DBRemoteUnlock
DBRemoteLockFinished DBRemoteKillAllLocks DBRemoteNotify
DBRemoteNotifyConflict DBRemoteRequestNotification DBRemoteQueryProgram
DBCancelRemoteQueryProgram DBRemoteUpgrade FMODBC::RCGetDSNList
FMODBC::RCODBCConnect FMODBC::RCODBCQuery FMODBC::RCODBCCommand`

plus `RCPerformScriptOnHost` (PSOS; no `CreationFunction` export, dispatch
path unknown). Transport: omniORB with `sslTP` (DBEngine imports
`omni::sslContext::*`) and `httpTP` (imports `omni::httpContext::proxy_url`,
`proxy_username`, `proxy_password`: HTTP proxy tunnelling of GIOP, inf).
ZIOP is linked on Mac but no ZIOP symbol is imported, and Windows ships no
ZIOP DLL, so GIOP compression is not used (inf). Authentication material:
`DBAuthenticationInfo` (user/password, NT/Kerberos SSO, OAuth,
`sFMIDTokenScheme`, `sAppleIDPasscodeScheme`) and Support `SRP_Client`
(inf: SRP password verification). Details belong to network-protocol.md.

**gRPC is not the FileMaker host protocol.** The only application gRPC
service compiled in is `/eap.v2.EntityAccessPipeline/{Create, Delete, Events,
Get, GetFile, Update}` (obs: method path strings, `eap::v2::EntityAccessPipeline::Stub`
and `::Service`). Its messages (`Entity`, `EntityDef`, `Query`,
`ConditionExpr`, `File`, `Event`) are the F5 protos. Callers are
`Draco::DAL_gRPC` and `Draco::DBMagnoliaSource` next to strings "Claris
Studio Data Source", "Link Claris Studio Table" (inf: "Magnolia" is the code
name of the Claris Studio data source, and these protos describe Claris
Studio, not FileMaker Server). The other gRPC services present are stock
(channelz, health, reflection, ALTS handshaker).

**Storage engine.** Page/frame cache (`HCacheMgr` client, `HSCacheMgr`
shared-memory server variant), B-tree keys (`HBAbstractKey`, `HBInt16Key`,
`HBInt32Key`, `HBSerialKey`, `HBCompositeKey`), positions (`HBPosition`,
`HPosInfo`), slots (`HBAMSlot`, `HPage::HSlotMatrix`), write-ahead log
(`HBLogFile`, `HBLogRecord`), transactions (`HDBTxn`, `HSubTxn`), encryption
at rest (`HBEncryptionInfo`, `HBEncryptionKeyBlock`,
`HBEncryptionChecksumBlock`, `HBEncryptionUserKeyCache`,
`HBEncryptedFileCopyTransformer`). The file magic `HBAM7` appears in
`FMStrs.dls` (section 2.5).

**Calculation engine.** All calc functions are methods of
`Draco::DBDataArg` (examples: `JsonSet`, `JsonGet`, `JsonParse`,
`JSONValueExists`, `ComputeCoreMLModel`, `GetLiveTextAsJSON`,
`GenerateLLMJSONLayoutSchema`). Numbers are `Draco::FixPt` (Support,
arbitrary-precision decimal, inf). JSON values are jsoncpp `Json::Value`
(obs: `Draco::DBData::ReplaceJsonValue(std::shared_ptr<Json::Value>)`);
jsoncpp stores object members in a sorted map, which would explain
FileMaker's alphabetical key order in JSON output (inf, verify).

**Script engine.** Runs in FMEngine (`ScriptRuntimeBase`, `ScriptStep`,
`ParameterListFactory`), shared with the server-side runtime
(`SMScriptRuntime`, `SMSession`, `SMScriptRuntime::IsServerAbort`). The
server has three runtimes on that base: `FMS::SASEScriptRuntime` in
`fmsased` (PSOS and schedules), `iwp::IWPScriptRuntime` in `fmscwpc`
(WebDirect) and `FMWIP::WIPScriptRuntime` in `fmwipd` (Data API).
`ScriptStep::IsServerCompatible`, `IsWebDirectCompatible`,
`IsWebCompatible` and `IsCustomWebCompatible` test the Script Workspace's
compatibility filter, not a step; per-step support is a table
([script-runtime.md](script-runtime.md), `catalogs/script-step-compat.csv`). Step XML (clipboard/Save as XML)
has two generations: `V1_ScriptStepXML`, `V2_ScriptStepXML`.

**Plug-in API.** FMWrapper exports 252 C functions in 12 families
(`FM_Text_*` 34, `FM_CharacterStyle_*` 33, `FM_DateTime_*` 31, `FM_FixPt_*`
28, `FM_Data_*` 27, `FM_BinaryData_*` 24, `FM_Color_*` 21, `FM_QuadChar_*`
15, `FM_ExprEnv_*` 15, `FM_DataVect_*` 15, `FM_RowVect_*` 5, `FM_Locale_*`
4). `FM_ExprEnv_*` covers `Evaluate`, `EvaluateGetFunction`,
`ExecuteFileSQL`, `ExecuteFileSQLTextResult`, `RegisterExternalFunction(Ex)`,
`RegisterScriptStep`, `SessionID`, `FileID`, path conversion. DBEngine
`Draco::DBPlugIn` loads plug-ins and resolves `FMExternCallProc` (obs string).
Plug-ins installed from containers are xar archives (`PluginHelper::CreateXarFile`,
`ExtractXarFile`). A Linux OpenMaker cannot load Mac/Windows plug-ins; the
ABI matters only if Linux-native plug-ins are ever targeted.

**AI/ML** (all in DBEngine unless noted). Providers and endpoints (obs
strings): OpenAI `https://api.openai.com/v1/`, Anthropic
`https://api.anthropic.com/v1/`, Cohere `https://api.cohere.com/v2/`,
Google `https://generativelanguage.googleapis.com/v1beta/`, Apple
`https://api.afm.aiml.apple.com/v1/` with default model
`afm-text-30b-instruct-latest` (present in the Windows DBEngine too, so this
is a hosted Apple service, not on-device), and "OpenSource" (custom
endpoints, `FMOpenSourceLLMRequest`, fine-tuning upload). Capabilities by
method name: chat with tools (`PerformLLMRequestWithTools`, roles
system/user/assistant/tool), streaming per provider, embeddings (single and
bulk, image embeddings via Cohere), fine-tuning (OpenAI and OpenSource),
RAG spaces (`AddRAG`, `RemoveRAG`, `PromptRAG`, `GetRagSpaceInfo`),
NL to SQL (`GetSQLQuery`, `generateDBSchema`), NL find
(`PerformLLMLayoutQuery`, `generateLayoutMetadataSchema`), script review
(`FMLLMScript::GenerateScriptCodeReview`), rate-limit handling
(`ParseRateLimitReset`), call logging (`LLMDebugLogger`). Semantic find
(`DBSemanticFind`, cosine similarity, threshold) can run on the host.
Local ML: `CoreMLModel` (Mac only; CoreML imported by DBEngine), in-house
`DecisionTree`/`RandomForest` regression (`Configure Regression Model`),
`FMVisionKit::GetLiveText`/`ReadQRCode` (Vision on Mac, ZBar on Windows for
QR). Tokenizer: tiktoken `cl100k_base` (`TokenEncoding`). UI:
`InsertEmbeddingSummary` (both platforms).

**Claris cloud.** Claris ID OAuth (`Draco::FMID`: login/logout URLs,
refresh tokens, organization selection, `GetDAPIRedirectURL`,
`GetWebDRedirectURL`), Claris Studio (eap.v2 gRPC and a REST query builder
`DAL_gRPC::MagnoliaQueryBuilder_REST`), Claris Connect flow triggering
(strings "Getting the list of teams and flows from Claris Connect..."),
Sign in with Apple passcode email (`RCSendAppleIdPasscodeEmail`), FileMaker
Cloud administration (`io::swagger::client::api::OrganizationApi`,
`DriveApi`, `ServerApi`). AWS SDK C++ (EC2, S3, KMS, SecretsManager, SQS,
IoT, Transfer, S3Encryption) and `Draco::FileBox` (S3 multipart transfers,
backup, `RegisterServerWithCloud`) are server/cloud code compiled into the
shared DBEngine (inf).

**Auto-update.** Mac: Sparkle 2 (`SPUUpdater`), EdDSA key in Info.plist,
appcasts `https://productupdates.filemaker.com/pro/REVISIONVER/release/fmpa_VERSION_BITS_OS_qgehuftqcc.xml`
(and `internal`/`ets` variants), in-product notifications
`https://productupdates.filemaker.com/inProductNotification.json`. Windows:
WinSparkle 0.7.0. Not needed on Linux (distribution packaging).

**Licensing.** License keys verified locally (`LicenseCode::Scheme1Verify`,
`Scheme3Verify`), license certificates (`.fmcert`,
`FMLicenseServices::FMLicenseCertStore`), license server requestor, trial
cookie, Windows `ZAuth.exe` to persist keys. Out of scope for OpenMaker;
noted only so later workers can skip it.

## 5. Catalogs (docs/re/catalogs/)

### 5.1 Where they live

The English catalogs are not in the Windows `FMRSRC.dll` or in Mac
`.lproj` files. They are compiled into DBEngine as a string-resource
directory (obs, Mac x86_64 slice, SHA-256 `b8bf96a6...958fb1`):

- Directory at VA `0x2556500` (file offset 39150848, `__DATA_CONST,__const`):
  37 entries of `{u64 resource_id, ptr records, u64 count}`, ids 129-165.
  It is referenced by code in `Draco::DBResourceCacher::SetParamText`
  (LEA at `0x1b6bea2`).
- Each record is four C-string pointers (32 bytes), encoded as
  `LC_DYLD_CHAINED_FIXUPS` rebase pointers (low 36 bits = target VA).
  For functions: `{name, signature, category_id, "Display Name|Description"}`;
  for script steps: `{name, category_name, description, ""}`.
- Function tables mark free or reserved slots with `#Used#`, `#NotUsed#`,
  `#UnUsed#`, `#Delete Me#`; index positions are preserved, which is why the
  index is believed to be the internal id (inf).
- The same strings exist in the Windows `DBEngine.dll` (spot-checked:
  "Perform Semantic Find", "Configure RAG Account ", "GetRAGSpaceInfo",
  "Cancel PDF", "OnWindowTransaction").

| Resource | VA of records | Count | Content |
|---|---|---|---|
| 129 | `0x2536580` | 2045 | general UI and message strings (includes script trigger names and descriptions) |
| 130 | `0x2546520` | 245 | calc functions, original id space 0-244 (ids 0-29 reserved: `#Used#` except 26 `#NotUsed#`, inf operators; 30-35 `not and or xor True False`) |
| 131 | `0x25483c0` | 142 | `Get()` selectors |
| 132 | `0x2549580` | 248 | script steps, index = step id |
| 133 | `0x254b480` | 14 | script step categories |
| 134 | `0x254b640` | 27 | Design functions |
| 135 | `0x254b9a0` | 23 | function categories (`category_id` n maps to entry n-1) |
| 136 | `0x254bc80` | 62 | index / sort languages |
| 137, 138 | | 32, 15 | obsolete byte functions, legacy `Status()` names |
| 139 | | 1 | `While` |
| 140-145 | | 18, 17, 24, 23, 8, 2 | newer functions grouped by parameter count: 140 variable/optional, 141 zero (and JSON/path type constants), 142 one, 143 two, 144 three, 145 four (inf from signatures) |
| 146, 147 | | 22, 20 | `GetLayoutObjectAttribute` attribute names, `objectType` values |
| 148, 149 | | 22, 22 | catalog kinds and catalog item kinds |
| 150-158 | | | misc UI lists, font names (154, 155), recovery log text (151), validation messages (153), `GetAVPlayerAttribute` names (158) |
| 159 | `0x2551560` | 283 | error codes and messages |
| 160 | `0x25538c0` | 248 | empty parallel table to script steps (unk purpose) |
| 161-165 | | | certificate status messages, `GetSensor` names (162), starter-solution names, patch tool log text |

FileMaker Pro 26.1.1's slice (SHA-256 `096a1994...`) has the directory at
VA `0x2574bd0` with the same 37 resources; the counts that changed are 129
(2,124), 131 (143), 132 (251), 140 (19), 159 (284), 160 (251) and 165 (29)
[O]. The catalogs stay at 26.0.3; what 26.1.1 adds is in
catalogs/README.md, "Added in 26.1.1".

### 5.2 Files

| File | Rows | Source |
|---|---|---|
| `calc-functions.csv` | 416 rows; 236 functions (131 base, 26 Design, 1 While, 75 newer, 3 Location), 52 keywords/constants, 47 obsolete, rest placeholders | DBEngine resources 130, 134, 137-145, 156 |
| `get-selectors.csv` | 142 rows, 140 named | resource 131 |
| `script-steps.csv` | 248 ids, 221 named (one internal `HACK_QUICKSTART_15`) | resource 132 |
| `error-codes.csv` | 283 | resource 159 |
| `script-triggers.csv` | 29 (incl. `Unknown`) | resource 129 indices 1221-1276 |
| `layout-object-types.csv` | 22 | resource 147 + FMEngine XML type table at VA `0x901f40` (`Draco::fieldType` ... `Draco::segmentedBarType`) + FMEngine/`FMLayoutPB` class names |
| `enumerations.csv` | 248 | resources 133, 135, 136, 146-149, 158, 161, 162 |
| `re/work/arch/dbengine-strres.tsv` (git-ignored, not in the catalogs) | 4,092 | raw dump of all 37 resources (`resource_id, index, f0..f3`; backslash escapes `\n \r \t \\`, and U+2014 written as the six characters `\u2014`) |

Function categories by count: Text 39, Design 26, Container 24, Logical 20,
Number 18, JSON 15, Artificial Intelligence 14, Japanese 12, Miscellaneous
11, Date 10, Aggregate 10, Text formatting 10, Trigonometric 9, Mobile 5,
Time 4, Financial 4, Repeating 3, Timestamp 1, Get 1. Script steps by
category: Miscellaneous 30, Control 28, Files 22, Fields 21, Artificial
Intelligence 17, Records 16, Windows 16, Open Menu Item 16, Navigation 13,
Found Sets 13, Spelling 8, Editing 8, PDF Files 7, Accounts 6. FileMaker
Pro 26.1.1 adds one AI function, one Control and one Fields step.

### 5.3 Catalog caveats

- Step ids: index = id agrees with step ids published in FileMaker's own
  XML formats (Perform Script 1, If 68, Set Field 76, Go to Related Record
  74, Insert from URL 160, Set Variable 141; external knowledge, re-verify
  against a clipboard or Save-as-XML capture); obs that the table is
  index-ordered with gaps kept as empty entries.
- Function ids: resolved in [calc-encoding.md](calc-encoding.md) section 2.1:
  the stored token is the string-resource index, with prefix bytes
  `f8`-`fe` selecting resources 139-145. Historical note:
  `Draco::gFunctionNames`, `gZeroArgFunctionNames`, `gFunctionPrototypes`,
  `gLocalizedFunctionNames` are runtime-built (`__common`). The FQL symbol
  `_FunctionTable` (VA `0x2585000`) is the SQL function table, not the calc one.
- Get selector numbers: the table index is given; whether the stored token or
  the plug-in `EvaluateGetFunction` selector equals the index (or index +
  an offset) is unverified.
- Trigger order is the order of the string table; numeric trigger ids are
  not established. `OnTimer` and `OnCallbackScript` have no description.
- Script step parameter signatures are not in these tables; they are built by
  `Draco::ParameterListFactory` (FMEngine) and serialized by
  `V2_ScriptStepXML`. Extracted in [script-steps.md](script-steps.md) and
  `catalogs/script-step-params.csv`.

## 6. Third-party components

| Component | Version | Platforms / binary | Evidence | Rust ecosystem equivalent |
|---|---|---|---|---|
| OpenSSL | 3.5.7 (9 Jun 2026) | both; dylibs / `libcrypto-3-x64.dll` | `strings libcrypto.3.dylib`; PE FileVersion | `rustls` + `aws-lc-rs` (or `openssl` crate if exact cipher parity is needed) |
| ICU | 76.1 | Mac static in Support (`icu_76::`), Win `icu{dt,uc,in}76.dll` | namespace `icu_76`, PE FileVersion 76,1 | `icu` (ICU4X) for most; `rust_icu` if byte-exact ICU 76 collation keys are needed for index compatibility |
| omniORB | 4.3.4 (Win build "4.3.4.f1"), omnithread 4.4 | both | file names, "omniORB version 4.3.x", PE FileVersion | none maintained; write a minimal GIOP/CDR client in Rust |
| gRPC core | 16.0.0 "guadalupe_river_park_conservancy" (gRPC 1.38 era) with protobuf, abseil `lts_20210324`, upb, re2, c-ares 1.14.0, xDS/envoy protos | DBEngine (static) | version strings next to gRPC code; `_lts_20210324` suffixes | `tonic` + `prost`; `hickory-resolver` |
| libcurl | 8.20.0-DEV | Mac static in DBEngine, Win `libcurl.dll` (+ `WLDAP32`) | "8.20.0-DEV" string; PE FileVersion | `curl` crate (libcurl) because Insert from URL exposes cURL options; `reqwest` for internal HTTP |
| libssh2 | 1.11.1_DEV | Mac static in DBEngine (SFTP for curl); Win inside `libcurl.dll` | "SSH-2.0-libssh2_1.11.1_DEV" | via `curl`; `ssh2` or `russh` if needed directly |
| AWS SDK for C++ | 1.11.367 (aws-crt-cpp) | DBEngine (static) | "1.11.367" after "Initialized AWS-CRT-CPP with version" | not needed for a client (`aws-sdk-*` if ever) |
| OpenLDAP | 2.6.7 | Mac `libldap`/`liblber` | build path `OPENLDAP_REL_ENG_2_6_7` | `ldap3` |
| Cyrus SASL | 2.1.23 (Win `libsasl.dll` + plugins), Mac system libsasl2 (expects 2.1.26) | both | PE FileVersion; libldap string | `rsasl` |
| iODBC | 3.52.13 | Mac DBEngine (static driver manager) | version string near `_iodbcdm_*` | `odbc-api` over unixODBC |
| libetpan | 1.9.3.14 | Mac static in FMEngine, Win `Libetpan.dll` | PE FileVersion; `_libetpan_get_version_*` | `lettre` (SMTP), `mail-builder` |
| libxml2 / libxslt / libexslt | 2.13.5 (Win); Mac uses system libs | FMEngine | PE FileVersion | `quick-xml`/`roxmltree`; XSLT 1.0 has no mature pure-Rust engine: bind libxslt (`libxslt` crate) |
| libiconv | 1.17 (Win) | | PE FileVersion | `encoding_rs` |
| zlib | 1.3.1 (Win `zlibwapi.dll`), 1.2.12 headers on Mac (system libz) | both | PE FileVersion; "1.2.12" next to gRPC compression | `flate2` (zlib-rs backend) |
| ANTLR4 C++ runtime | version unk (`antlr4::RuntimeMetaData::VERSION` symbol) | DBEngine (FQL grammar) and FMEngine (second copy) | symbols | hand-written parser or `sqlparser` with a custom dialect; `antlr-rust` is unmaintained |
| Boost | version unk (asio, thread) | all | `boost::asio::*`, prebuilt path `Boost/working` | `tokio`, `std::thread` |
| jsoncpp | unk | DBEngine, FMP (`Json::Value`, `Json::Reader`) | symbols | `serde_json` (BTreeMap ordering to match sorted keys) |
| nlohmann json, jsoncons, rapidjson, cJSON, jwt-cpp, tinyxml2 (inside AWS) | unk | DBEngine/FMEngine | symbols | `serde_json`, `jsonwebtoken` |
| moodycamel ConcurrentQueue | unk | DBEngine, FMEngine | symbols | `crossbeam` |
| Howard Hinnant `date`/tz | unk | Support, DBEngine | `date::time_zone`, `date::tzdb_list` | `jiff` or `chrono-tz` |
| moment.js + moment-timezone | 2.29.4 | Mac `jsrunner.html` | file header | `jiff` |
| Skia | m126 (`skia.dll` 126.0.0.0) | Windows only | PE FileVersion | `skia-safe` (closest parity with Windows rendering), alternatives `tiny-skia`, `vello` |
| PDFium | 132.0.6957.0 | Windows only (Support, FMP) | PE FileVersion | `pdfium-render` |
| PDFKit (system) | | Mac only (Support) | dylib link | `pdfium-render` on Linux |
| PDFHummus (PDF-Writer) + FreeType + libjpeg + libtiff | unk | Mac FMP static | `PDFHummus::DocumentContext`, `_FT_*`, `_jpeg_*`, `TIFF*` | `krilla` or `printpdf`/`lopdf` for writing; `skrifa`/`swash` for fonts; `image` |
| ChartDirector | 7.0.1 | both | PE FileVersion; `libchartdir.7.dylib` | `plotters` or a custom chart renderer on the chosen 2D backend |
| Codejock Xtreme ToolkitPro | 17.2.0 | Windows UI | PE FileVersion | n/a (UI toolkit choice: e.g. `slint`, `iced`, `gtk4`, or custom on `winit` + Skia) |
| MFC 14 / WPF (.NET 4.8) / WebView2 1.0.1661.34 | | Windows | imports, assembly refs | `wry` (WebKitGTK on Linux) for the web viewer |
| WKWebView, Cocoa, Swift runtime | | Mac | dylib links | `wry` |
| Sparkle 2 / WinSparkle 0.7.0 | | Mac / Win | Info.plist, PE FileVersion | not needed (distro packages); `self_update` if desired |
| ZBar | 0.10 (`libzbar64-0.dll`) | Windows QR reading | PE ProductVersion | `rqrr` |
| Apple Vision / CoreML / CoreImage | | Mac (DBEngine, Support) | dylib links, `CoreMLModel`, `FMVisionKit` | `ort` (ONNX Runtime) or `candle` for models; `ocrs`/Tesseract for Live Text |
| ProofReader spell engine | 22.0.1.0 (Win DLL); same `PD*`/`PR*` C API statically in Mac FMP | both | PE FileVersion; Mac symbols `_PDDC*`, `_PDRDinit` | `spellbook` or `hunspell-rs` (FileMaker `.mpr` dictionaries are a proprietary format) |
| xar (libxar) | system on Mac; Windows unk | FMEngine plug-in packaging | `_xar_open`, `PluginHelper::ExtractXarFile` | minimal xar reader (simple format) if plug-in containers matter |
| tiktoken BPE `cl100k_base` | | both (`Tiktokens/`) | resource file, `TokenModelGenerator::Cl100KBase` | `tiktoken-rs` |
| CBOR (aws-c-common) | unk | DBEngine | `_cbor_*` | `ciborium` |

Not found: SQLite, ONNX Runtime, DirectML/WinML, Hunspell, PCRE, LZ4, Zstd,
Brotli (searched symbol names and strings).

## 7. Client vs server responsibility

The client links the complete engine (calc, script, SQL, storage), and
remote operations are fine-grained. Evidence (obs symbols) with the
resulting split (inf):

| Operation | Evidence | Where it runs when the file is hosted |
|---|---|---|
| Record data access | `RCDownloadList`, `RCDownloadWithLock`, `RCDownloadFromHost`, `RCDownloadFromTemp`, `PrefetchRows`, `DBTableProxy::ResyncTempFile`, `DBTableProxy::ValidateCachedRecordsAndGatherModCounts`, `RCCompareModCounts`, `HFrame::isServerTempDB`, `Get(CacheFilePath)` | Client keeps a local record cache (inf: an HBAM temp file) validated by modification counts |
| Record create / edit / commit | `RCCreateRecord`, `RCUploadWithLock`, `RCCommitRecords`, `DBRemoteLock`/`Unlock`/`LockFinished`/`KillAllLocks`, `DBRemoteNotify`, `DBRemoteNotifyConflict` | Edited on the client, locked and committed on the host; host pushes change notifications |
| Index maintenance | `RCBuildIndex::HostBuildIndex`, `HostTableCommit::UpdateIndex`, `UpdateIndexModify` | Host |
| Calculations (stored, unstored, auto-enter, validation) | full calc engine in client DBEngine (`DBCalcSolver`, `DBDataArg`); no remote calc operation except `RCGetCalcGuestCount` | Client (stored-calc timing on commit unverified) |
| Portal filters | `DBRelatedSet::ApplyClientOnlyFilterCalc` | Client |
| Finds | `DBRemoteQueryProgram` (`Execute`, `PerformOperation`, `Abort`), `DBCancelRemoteQueryProgram`, `DBFindRowSolver::SolveRemotePath` vs `SolveOneLocalTableOfPath`, `DBQueryPath::SetRemoteProgram` | Host executes a compiled query program when possible; local fallback exists |
| Sorting | `DBSortSpec::CanSortOnHost`, `FMLayout::CanRemoteSort`, `DBQueryProgramRunner::RemoteSort` | Host when the sort spec allows, otherwise client |
| Summary fields | `DBSummary::CanSummarizeOnHost`, `DBQueryProgramRunner::RemoteGetSumValue`, client `SummaryCompute` | Host when allowed, otherwise client |
| Semantic find | `DBSemanticFind::CanFindOnHost`, `DBQueryProgramRunner::RemoteSemanticFind` | Host when allowed |
| ExecuteSQL | `FQL::ExecuteSQLCalc`, `FQL::Query::Execute` in client DBEngine; finds, index lookups, temporary relationships and temporary result tables ([execute-sql.md](execute-sql.md) 10) | Client: nothing SQL-specific runs on the host; data access through the ordinary remote find and download machinery (inf from code) |
| Scripts | `ScriptRuntimeBase` in client FMEngine; PSOS via `RCPerformScriptOnHost` (`StartScript`, `ScriptFinished`, `SendScriptResultToClient`) and `RCSendPSOSRetToClient`; `ScriptStep::IsServerCompatible` | Client, except Perform Script on Server |
| Global fields and variables | `NetHostSession::ClearGlobals`, `NetHostInternalFileUser::GetGlobalVars`, `Draco::KGlobalClientVars` | Per-session state on the host (inf) |
| ODBC (ESS, Execute SQL step) on hosted files | `FMODBC::RCODBCConnect`, `RCODBCQuery`, `RCODBCCommand`, `RCGetDSNList` | Host (DSNs on the server) |
| Containers | `RCUploadBinaryData`, `RCTransferContainer`, `RCVerifyContainer`, `RCGetContainerURL`, `RCDownloadThumbnail`, `DBBinaryData::RemoteHeader` | Stored on host, transferred on demand |
| AI calls (LLM, embeddings) | `FMLLM::*` and curl in client DBEngine | Client calls providers directly unless run via PSOS (inf) |
| Execute FileMaker Data API step | `HLClient`, `FMWIP::WIPJSON` in client FMEngine | Client |

Consequence for OpenMaker: even a hosted-only client needs the full calc
engine (all functions in section 5), the script engine, sort and summary
fallbacks, a local find evaluator, the FQL SQL engine, JSON functions, a
record cache with mod-count validation, and the lock/commit/notification
protocol. The storage engine is needed for local files and possibly for the
cache (OpenMaker may choose its own cache format).

## 8. Corrections and refinements to README findings

| Finding | Refinement | Type |
|---|---|---|
| F1 | ZIOP is Mac-only and unused: DBEngine imports 0 symbols from `libomniZIOP*`; Windows ships no ZIOP DLL. `sslTP` and `httpTP` are used (HTTP proxy settings). | obs + inf |
| F4 | gRPC core 16.0.0 (1.38 era), abseil lts_20210324, c-ares 1.14.0. libssh2 comes with a static libcurl 8.20.0-DEV on Mac (SFTP); on Windows both live in `libcurl.dll`. | obs |
| F5 | The DBEngine protos are the Claris Studio `eap.v2.EntityAccessPipeline` gRPC API, not the FileMaker host protocol. | obs + inf |
| F7 | Windows UI = MFC + Codejock + WPF via C++/CLI `Interop.dll`; `ClientUI.dll` holds cross-platform UI models that are inside the main executable on Mac. | obs |
| F8 | Skia and PDFium are Windows-only. Mac draws with CoreGraphics (`CGGraphics`), writes PDF with PDFHummus + FreeType, reads PDF with PDFKit. | obs |

## 9. Notable surprises

1. DBEngine/FMEngine contain FileMaker Server and Cloud code: AWS SDK
   clients (EC2 alone 13.7k symbols), shared-memory page cache, Data API JSON
   engine, server script sessions, Claris Cloud admin OpenAPI client, S3
   `FileBox`.
2. Localized string tables (`FMStrs.dls`) are HBAM7 files, the `.fmp12`
   container format.
3. Newer calc functions are grouped into tables by parameter count.
4. An Apple-hosted LLM endpoint (`api.afm.aiml.apple.com`) is a first-class
   provider on both platforms.
5. The ANTLR4 runtime is linked twice: DBEngine (FQL grammar) and FMEngine,
   which exports no generated grammar classes (`XPathLexer` belongs to the
   runtime), so its use there is unknown. The server build ships it as
   `libantlr4-runtime.so.4.13.1`.
6. Usage analytics (`ProAnalytics*_RCp1`) upload from the client.

## 10. Unknowns and suggested next steps

Unresolved:

- Get selector numbering and trigger numeric ids.
- External object `typeID` values for web viewer and chart.
- Why FMEngine carries its own ANTLR4 runtime (no exported grammar).
- How `RCPerformScriptOnHost` is dispatched without a `CreationFunction`.
- What resource 160 (empty, 248 entries) is for.

Priorities:

1. Network workstream: decode the `Perform` payload envelope and the
   `CreationFunction` registry (operation id per remote class); that unlocks
   the record cache protocol in section 7.
2. Calc workstream: recover the token encoding of stored formulas, using
   `DBCalcParse`, `DBCalcFormula`, `DBFunctionCatalog`, `M_DBFunctionIndexList`
   and the catalogs here; verify jsoncpp key ordering against a live server.
3. Script workstream: extract `ParameterListFactory` schemas and match them
   to `V2_ScriptStepXML` names to complete `script-steps.csv`.
4. File-format workstream: use `FMStrs.dls` and
   `DynamicTemplates/Assets/*.fmp12` as HBAM7 samples.
5. Decide the 2D stack early: Skia (`skia-safe`) gives the closest match to
   Windows rendering; CSS themes (`Themes/*/*.css`) define styling.

## Appendix: reproduction

Scripts (read-only parsers, no target code executed):

- `tools/arch_cluster.py <dsym.txt> <out.json>`: namespaces, class scopes,
  C prefixes per binary.
- `tools/arch_subsystems.py re/work/arch re/work/arch/subsystems.json`:
  subsystem assignment.
- `tools/macho_ptrscan.py <thin> find|dump ...`: chained-fixup pointer search.
- `tools/macho_strtables.py <thin> scan|export ...`: string-pointer arrays.
- `tools/macho_xref.py <thin> <lo> <hi> --symbols <dsym>`: RIP-relative
  references (heuristic).
- `tools/dbengine_strres.py re/work/thin/DBEngine 2556500 <out.tsv>`:
  string-resource directory export.
- `tools/make_catalogs.py <strres.tsv> docs/re/catalogs`: catalog CSVs.
- `tools/pe_res_dump.py <pe> <rea-json> <outdir>`: PE resource payloads and
  RT_STRING decoding from `rea inspect-pe-resources` output.

Scratch outputs: `re/work/arch/` (demangled symbols, cluster JSON, import
lists, REA JSON for `WindowsUI.dll`, `Interop.dll`, `FMRSRC.dll`).
