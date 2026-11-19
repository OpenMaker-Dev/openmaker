#!/usr/bin/env python3
"""Assign clustered class scopes (from tools/arch_cluster.py) to subsystems.

Rules are ordered regexes over the class scope (namespace-qualified, templates
stripped) and over C-symbol prefixes. First match wins. Output: markdown-ish
text summary per subsystem with per-binary symbol counts and top classes, plus
a JSON with the full assignment and the largest unclassified scopes.

Usage: python3 -I tools/arch_subsystems.py <cluster-dir> <out.json> [bin ...]
"""
import collections
import json
import re
import sys

R = [
    # ---- third-party (keep before first-party catch-alls)
    ("3p:AWS SDK for C++", r"^(Aws|smithy)(::|$)|^aws$"),
    ("3p:gRPC/protobuf/abseil/upb/re2/c-ares", r"^(grpc|grpc_core|grpc_event_engine|google|absl|re2|tsi|GrpcUdpListener|grpc_[a-z_]+)(::|$)|^(grpc|gpr|upb|envoy|xds|udpa|validate|alts|gsec|ares|aresx|descriptor|address|tsi)$"),
    ("3p:ANTLR4 runtime", r"^(antlr4|antlrcpp)::"),
    ("3p:Boost", r"^boost::"),
    ("3p:C++ std library", r"^std::"),
    ("3p:ICU 76", r"^icu_76::|^(u|uprv|ucnv|ucol|uset|uregex|uhash|ucal|ures|uloc|ubidi|uspoof|udat|unum|utext|usearch|udatpg|vzone|ubrk|unorm2|utrans|utrie2|ulocbld|udata|uscript|ulocimp|ucsdet|ucurr|uidna|ztrans|izrule|uldn|ulist|ulocale|utrie|ucase|ucasemap|ufmt|unumf|uregion)$|ulocimp_|uspoof_"),
    ("3p:JSON libs (jsoncpp, nlohmann, jsoncons, rapidjson, cJSON, jwt-cpp)", r"^(Json|nlohmann|jsoncons|rapidjson|jwt)::|^cJSON$"),
    ("3p:libssh2/OpenLDAP/iODBC/CBOR", r"^(libssh2|ldap|ber|ldif|iodbcdm|cbor|DM|dm|DRV|SQL|SQLCol|SQLGetConnect|SQLGetDesc|SQLGetDiag|SQLSetConnect|SQLGet|SQLGetStmt|SQLSetStmt|mqtt|trace|FLAGS)$"),
    ("3p:libetpan (mail)", r"^(mailimap|mailimf|mailmime|mailstream|newsfeed|newsnntp|mailprivacy|mail|mailpop3|mailmbox|mailmessage|mailsession|mailsmtp|imap|libetpan|mailmh|mmap|maildir|mailfolder|mailesmtp|nntpdriver|chash|clist|mailstorage|lep|maildriver|mbox|pop3|carray|nntp|mboxdriver|mh|mhdriver|pop3dr|yy|yyget|generic|newsnntp)$|^fm_etpan::"),
    ("3p:xar", r"^(xar|fm_xar_iter)(::|$)"),
    ("3p:FreeType/libjpeg/libtiff", r"^(FT|ft|af|jpeg|jinit|TIFF|TIF|TIFFHashSet|TIFFSwabArrayOf|tt|t1|cff|ps)$"),
    ("3p:PDFHummus (PDF writer/parser)", r"^(PDFHummus|PDFParser|PDFParserTokenizer|CharStringType2|Type1|CFF|TrueType|ObjectsContext|AbstractContentContext|PDFDocumentHandler|TIFFImageHandler|PDF[A-Z][A-Za-z]*Writer|DocumentContext|PageContentContext|IByte|InputFile|OutputFile|PDFObject|PDFDictionary|PDFArray|Type1ToType2Converter|Type1ToCFFEmbeddedFontWriter|CFFEmbeddedFontWriter|TrueTypeEmbeddedFontWriter|CFFFileInput|Type1Input|CharStringType2Flattener|CharStringType2Interpreter|FreeTypeFaceWrapper|FreeTypeOpenTypeWrapper|FreeTypeType1Wrapper|IFreeTypeFaceExtender|ByteListVectorImpl|IPageEmbedInFormCommand|PageEmbedInFormWithPageBox|InputStringBufferStream|OutputStringBufferStream)"),
    ("3p:moodycamel/misc", r"^(moodycamel|date|tools)::"),
    ("3p:Objective-C runtime metadata", r"^OBJC$"),
    # ---- first-party subsystems
    ("AI/ML (LLM, embeddings, semantic find, CoreML, tokenizer)", r"FMLLM|LLMDebug|CoreML|DecisionTree|RandomForest|SemanticFind|TokenEncoding|TokenModelGenerator|VectorMath|FMVisionKit|Embedding|RAG|PromptTemplate|NaturalLanguage|FineTune|AIAccount|AICall"),
    ("Claris Studio/cloud/Claris ID", r"FileBox|Magnolia|DAL_gRPC|^eap::|iStudio|CloudFactory|FMID|OCPToken|CSEventRequestor|^io::swagger|FMFileTransfer|AppleSignIn|SignIn|ClarisConnect|ClarisStudio|Studio(Token|Data)|CustomApp|LaunchCenter|KeyStorage"),
    ("Client-server networking (CORBA RCFMP12, remote ops, sessions)", r"RCFMP12|^RPO_i|^Draco::(RC[A-Z]|Net[A-Z]|DBRemote|DBCancelRemote|ORBConfig|IPAddress|IPv6Address|NetHost|NetGuest|FMSSLContext|OpenSSL|DBSession|DBBackChannel|ServerPicker|OpenRemote|FavoriteHosts|HostEntry|FMSHttpContext)|^(ServerPicker|OpenRemote|FmNetSharing|Hosts|HostRepresentation|UploadingModel|UploadItemNode|CommonUpload)|SRP_|RemoteContainer"),
    ("xDBC/ODBC external SQL sources (ESS)", r"Odbc|ODBC|^FMODBC::|FMSimpleODBC|DSN|xDBC|SQLQuery|SQLEdit|SQLWhere"),
    ("SQL engine (FQL, ExecuteSQL)", r"^FQL|^FQLSQL|DDLGenerator|XPathLexer"),
    ("Plug-in API", r"PlugIn|Plugin|^fmx::|^FM$|ExternalFunction"),
    ("Web publishing / Data API engine (FMWIP, HLClient)", r"^(FMWIP|HLClient|HLUtils|HLSession|HLTransactionMgr)(::|$)"),
    ("Insert from URL / HTTP / web services", r"FMURL|WebServiceRequestor|FMWS_|HTTPHeader|InsertFromURL|OAuthService|OAuthModel|FMWebServices|CurlHandle"),
    ("Web viewer / JavaScript", r"WebViewer|WKWebView|WebView|JSRunner|JavaScript|ProWebViewerView"),
    ("Charts", r"FMChart|ChartDirector|Chart[A-Z]|^Chart"),
    ("Accounts/privileges/security/encryption", r"DecryptionHelper|EncryptionHelper|Privilege|DBAccess|UserAccount|DBPassword|DBAuth|Authentication|HBEncryption|PasswordHash|PasswordKeychain|Certificate|ConvertEncrypted|^Draco::EAR|EAR[A-Z]|AccountAdmin|AddAccount|SecurityAccess|Keychain|Crypt|SHA256|CRCContext|AES|RSA|Cipher"),
    ("Licensing", r"Licen[cs]e|LicenseCode|LCVerify|TrialCookie"),
    ("Auto-update & in-product notifications", r"Sparkle|SoftwareUpdate|AppCast|Appcast|SPUUpdater|UpdateCheck|ProductUpdate|InProductNotification"),
    ("Usage analytics", r"Analytics"),
    ("Email", r"CMail|SendMail|CSendMail|MailSettings|SMTP|EmailAttachment"),
    ("Spelling", r"Spell|SPELL|Dictionar"),
    ("Charts/printing/PDF output", r"FMPDF|Hummus|PDF|Print|PageFormat|XViewPrint|XPrintInfo|PageSetup|Preview"),
    ("Import/export & file conversion", r"FMImport|FMExport|Excel|OOXML|ooxml|DataTransfer|Import|Export|FileFilterSpec|OLEFile|dBase|DBase|Convert(Mapping)?Model|FMBatchFile|SaveRecordsAs|JSONL"),
    ("XML formats (fmxmlsnippet, Save as XML, DDR)", r"XML|Xml|^DDReport|libxmlAdapt|FMFSlibxmlAdapt|FMXMLAdapt|FM_XSL|XSLT|SaXML"),
    ("Schema object transfer (copy/paste between files)", r"ObjectTransfer|PasteStyle|CopiedObjectStyle"),
    ("Add-ons, templates, patches", r"Addon|AddOn|DynamicTemplate|PatchGeneration|TemplateMetadata|TempAddonStorage|Patch"),
    ("Script engine", r"Script|SMSession|SMFileUser|^Draco::SM[A-Z]|Trigger|ParameterListFactory|ListParameter|Undo|CommandManager"),
    ("Calculation engine", r"DBCalc|DBDataArg|DBFunction|DBEval|^Draco::DBLet$|^Draco::DBCase$|^Draco::DBIf$|^Draco::DBChoose$|^Draco::DBLoop$|DBVarArgs|DBConstantArg|DBListArg|DBFieldRefArg|DBPolishEnv|DBBindingEnv|DBVarMap|DBGetFunction|DBTime(Parser|Pattern|Regex)|DBExpression|CalcModel|CalcDialog|Calculation|FieldCalc|FixPt|ApplyFormat|NumData|CustomFunction|DBFilterCalc|DBOperation|DBDependency|DBDepend"),
    ("Database engine: records, finds, sorts, summaries, relationships", r"^Draco::DB(Cursor|RecordCursor|RequestCursor|ResultSet|RowSet|RecordSet|RidList|AbstractRidList|RidListIter|Finder|FindRow|FindRowSolver|FindParser|SortTree|SortSpec|Summary|Histogram|QueryProgram|QueryPath|QueryProgramRunner|RelatedSet|Relation|JoinPred|PredicateBuilder|Cascader|TableProxy|Transaction|Lock|LockMgr|LockInfo|LockTable|LockUser|LockMemMgr|LockError|InternalLock|LockContinueThread|Notifications|RequestSet|CustomOrderIterator|KeyIterator|IndexSet|Data|DataSource|BinaryData|BinaryDataReceiver|Field|FieldSpec|FieldError|ValueList|ValueListIter|ValueListData|CustomValueListIter|FieldValueListData|CalculatedValueListData|Table|TableMgr|TableAlias|Schema|MemberTypedIterator|NameIterator|EngineController|Logger|Mesg|MesgAddr|Error|Bitmap|FontEntry|FontMapper|PermittedListManager)$|^Draco::(SummaryCompute|PrefetchRows|IndexList|DeferredJob|DeferredRemoteFile|TransferLogUpdate|DBRecord[A-Za-z]*|DBRow[A-Za-z]*|DBSort[A-Za-z]*|DBFind[A-Za-z]*|DBQuery[A-Za-z]*|DBLock[A-Za-z]*)(::|$)|SavedFinds|RecentFinds|FindReplace|FindRequest|^SummaryCompute"),
    ("Database engine: schema catalogs (tables, fields, TOs, value lists, files)", r"^Draco::DB[A-Za-z]*Catalog|^Draco::DB(Catalog[A-Za-z]*|CatalogMember|CatalogCommon|CatalogItemList|MetaTree|File|FileUser|FileReference|FileRefCatalog|BaseDirectory[A-Za-z]*|Library|LibraryMember|Resource|ResourceCacher|PersistentStore|PStoreCatalog|FileThumbnails|FileUserClarisCreatorStamp|TableAliasCatalog|FieldCatalog|DataSource[A-Za-z]*)$|^Draco::DB[A-Za-z]*Ex(Base)?$|FileBox|^Draco::DBFile[A-Za-z]*"),
    ("Storage engine (HBAM pages, B-trees, cache, log, recovery)", r"^Draco::H[A-Z]|^Draco::DBIndex|^Draco::DBKey|Recover|^Draco::DBFileConverter|FileSet|DiskCache|BloomFilter|^fmfs_imp::|^fmfs(_adapt)?(::|$)|^FMFS|^fs::|BlobProxy|SwapIStream"),
    ("Layout model, styles, themes (FMEngine)", r"^FMLayoutPB::|^FMStyle::|Layout|Theme|Style|^Draco::FMWindowModel|DataEntry|ActiveObject|LVPart|ListViewPart|TabOrder|Guide|SlideMgr|Popover|Portal|ButtonBar|SegmentedBar|CSSParser|PropertyValue|PropertyMap|PropertySet|^Draco::Part|LayoutPart|ObjectStyle|BaseStyle|StyleMetrics|ImageMemoryManager|Conditional"),
    ("View system & rendering (libViewSystem/ViewSystem.dll, text layout)", r"^(IView|BaseView|CGGraphics|ScrollView|GenericGraphicsRecording|IGraphics|ClipGraphics|EventSystem|CGImageVS|NativeScrollerView|ITopLevelViewBackEnd|ShadowView|ViewSystem|IImageRepresentationImpl|CVScrollBar|PlatformViewScrollerMac|BinnedDirty[A-Za-z]*|AlphaSpriteImage|Animation|IMouseEvent|LegacyNativeView|IImageRepresentation|IImage|Concrete[A-Za-z]*Event|IGraphicsStateTracker|PlatformViewMac|CVScrollableContent|BaseBackedView|NullTextImplementation|ALPHASPRITE|CopyCGImageFromCG)|View$|View::|Renderer|Render|^X(Text|InteractiveText|ITextUnit|TextLine|TextChunk|TextModel|Scrap|Wnd|Menu|Rect)|TextView|TextContent|CSSBlock|CSSBox|Graphics|Glyph|Font|^SCRAP$|SVG|OpenType|igColor|^XDrag|^XApp|^XList|ListPicker|^XStd|^XEdit|^XDrop|^XLink|^XIndexed|^XWord|^XRTF|^XMDI|^XRegistry|MultiRowView|Gutter"),
    ("Manage Database / relationship graph UI", r"^JGE|FMDefine|DefineFields|DefineRelation|DefineTables|TableAlias|RelationDialog"),
    ("Developer tools (Data Viewer, debugger, DDR, profiler)", r"FMDevTool|ScriptDebugger|WatchViewer|Profiler|Debugger|DataViewer"),
    ("Text, Unicode, locale (Support)", r"^Draco::(Text|TextBuffer|TextIterator|unistring|International|[A-Z]{2,3}(_[a-z]+)?_International[A-Za-z_]*|Saved_International|UnicodeRaw_International|System_International|Locale|SCSU[A-Za-z]*|DotStringsManager|ParagraphStyle|CharacterStyle|BaseCharacterStyle|SystemCharacterStyle|SubjectUnistring|TokenEncoding)$|^Draco::[A-Z]+(_[A-Za-z]+)?_International|^Unicode::|^Draco::.*International|^(DBMagnoliaTZHelper|DBObjectTags)"),
    ("Media & containers (images, audio/video metadata, thumbnails)", r"Image2D|ID3|MP4|MPEG|AVPlayer|AudioMetadata|PListAudio|Thumbnail|Container|Camera|BarCode|Barcode|InsertFromDevice|Signature|Picture|Image"),
    ("Platform & OS utilities (files, threads, IPC, prefs)", r"^Draco::(QuadChar|UniversalPath|File|Directory|Thread|DateTime|Debug|CacheEntry|ApplicationInfo|ByteBuffer|Machine|Coord[A-Za-z0-9]*|TaskHandler|Task|Timer|BitList|BitChunk|LockableListBaseClass|LongWaitMutex|NotificationSubject|XRect|ShmSegment|MsgQ|Process|SharingLock|SharingLockHolder|DataTypeDetector|PropertyList|RefCountedObj|RunLoopThread|CMutex|DispatchQueueGroupBase|ParallelMultiTaskHandler|KeyValueStore|KeyBuilder|unifile[A-Za-z]*|DeleteAllListFunctor|RepThread|SystemFontManager|SystemFontID|Error|ErrorDetail|ErrorString|ExtendedError|ErrorCount|DebugStreamMsg|RefCountedObjPtr)$|^ProcessInfo::|Preference|PrefPage|Settings"),
    ("Application shell & UI (windows, menus, dialogs, inspector)", r"^(FMDocWindow|CFMProApp|DocWin|DocWindow|DSDocWindow|FMDocWindowTopLevelViewMac|TopLevelViewMac|WinControllerMac|CMac|XCocoa|CocoaMenuSet|CocoaGetMenu|CocoaSet|Cocoa|Pro_ButtonBar|ClientUI_|PropBase|Prop$|PropProxy|PropGroupBase|SelectionPropMgr|SelectionViewManager|NativeScript|FMEditCustomMenu|FMCustomMenu|CustomMenu|Menu|ControlMgr|Inspector|FMCatalog|CatalogTree|FMAppSession|FMAppFileUser|FMUserAccount|NotificationConsolidator|FormattingStatus|DSAppearance|DSScrollBar|IGradientControl|OSL|AE|SnappingManager|GuideViewManager|ScrollerViewController|RulerViewController|CVRulerView|CVContentView|ScrollableViewStackManager|EditWorkspaceModel|FieldPicker|ManageContainers|ManageLayouts|ManageThemes|InsertEmbedding|PartManager|PartsView|DocViewController|ProLayoutViewController|C[A-Z][A-Za-z]*(Dlog|Dialog|Page|Sheet|Wnd)|[A-Za-z]*(Dlog|Dialog|Model|Controller|Window|Popover|Page)$)|^Draco::FM(Session|FileUser|CustomMenu[A-Za-z]*|TranslationData|TranslationProfile|DevToolNode)$"),
]
RULES = [(name, re.compile(rx)) for name, rx in R]


def classify(scope):
    for name, rx in RULES:
        if rx.search(scope):
            return name
    return "unclassified"


def main():
    cdir, out = sys.argv[1], sys.argv[2]
    bins = sys.argv[3:] or ["DBEngine", "FMEngine", "Support", "FileMakerPro", "libViewSystem", "FMWrapper"]
    table = collections.defaultdict(lambda: collections.Counter())
    tops = collections.defaultdict(lambda: collections.Counter())
    uncl = collections.Counter()
    for b in bins:
        d = json.load(open(f"{cdir}/cluster-{b}.json"))
        for scope, n in d["classes"]:
            s = classify(scope)
            table[s][b] += n
            tops[s][f"{b}:{scope}"] += n
            if s == "unclassified":
                uncl[f"{b}:{scope}"] += n
        for pfx, n in d["c_prefixes"]:
            s = classify(pfx)
            table[s][b] += n
            tops[s][f"{b}:{pfx}*"] += n
            if s == "unclassified":
                uncl[f"{b}:{pfx}*"] += n
    rows = sorted(table.items(), key=lambda kv: -sum(kv[1].values()))
    res = []
    for s, c in rows:
        res.append({"subsystem": s, "total": sum(c.values()), "per_binary": dict(c),
                    "top": tops[s].most_common(12)})
        print(f"{s}\t{sum(c.values())}\t" + " ".join(f"{b}={c[b]}" for b in bins if c[b]))
        print("    " + ", ".join(f"{k}:{v}" for k, v in tops[s].most_common(10))[:600])
    json.dump({"subsystems": res, "unclassified_top": uncl.most_common(300)}, open(out, "w"), indent=1)


if __name__ == "__main__":
    main()
