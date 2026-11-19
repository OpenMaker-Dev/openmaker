"""Research annotations for tools/calc_function_spec.py: tiers, host
capabilities, engine implementation symbols, server notes, compatibility
overrides and engine-derived notes and test vectors. Evidence for each
engine claim is in docs/re/calc-functions.md.

Tier: 1 = pure (no record, schema or host), 2 = needs record/schema/session
context, 3 = bound to a host, platform, device, UI or external service.
`core` marks the tier-1 functions common solutions depend on.
"""

# ---------------------------------------------------------------- functions
TIER3_FUNCS = {
    # container analysis / OS frameworks
    "GetContainerAttribute", "GetHeight", "GetWidth", "GetThumbnail", "VerifyContainer",
    "ReadQRCode", "GetLiveText", "GetLiveTextAsJSON", "GetTextFromPDF",
    # AI providers and models
    "ComputeModel", "GetModelAttributes", "PredictFromModel", "GetEmbedding", "GetRAGSpaceInfo",
    "GetTableDDL", "GetFieldsOnLayout",
    # Japanese input specifics
    "Furigana",
    # layout / UI
    "GetLayoutObjectAttribute", "GetLayoutObjectOwnerInfo", "LayoutObjectUUID", "WindowNames",
    # mobile
    "GetAVPlayerAttribute", "GetSensor", "Location", "LocationValues", "RangeBeacons",
    # host file system
    "ConvertFromFileMakerPath", "ConvertToFileMakerPath",
    # add-ons
    "GetAddonInfo",
}
TIER2_FUNCS = {
    "Average", "Count", "List", "Max", "Min", "StDev", "StDevP", "Sum", "Variance", "VarianceP",
    "Extend", "GetRepetition", "Last",
    "GetNthRecord", "GetSummary", "GetField", "IsValid", "Lookup", "LookupNext", "Self",
    "ExecuteSQL", "ExecuteSQLe", "Evaluate", "EvaluationError", "IsValidExpression",
    "GetFieldName", "GetBaseTableName", "GetPersistentData", "ListPersistentDataIDs",
    "GetRecordIDsFromFoundSet", "NPV",
}
# tier-1 functions that common solutions do not depend on
TIER1_NONCORE = {
    "GetAsCSS", "GetAsSVG", "DayNameJ", "Hiragana", "KanaHankaku", "KanaZenkaku", "KanjiNumeral",
    "Katakana", "MonthNameJ", "NumToJText", "RomanHankaku", "RomanZenkaku", "YearName",
    "FV", "PMT", "PV", "Acos", "Asin", "Atan", "Cos", "Degrees", "Radians", "Sin", "Tan",
    "StDev", "Combination", "Factorial", "Lg", "Ln", "Log", "Exp",
    "CryptGenerateSignature", "CryptVerifySignature", "CryptEncrypt", "CryptDecrypt",
    "CryptEncryptBase64", "CryptDecryptBase64", "TextEncode", "TextDecode", "Base64EncodeRFC",
    "CryptAuthCode", "HexDecode", "Base64Decode",
    "AddEmbeddings", "SubtractEmbeddings", "NormalizeEmbedding", "CosineSimilarity",
    "GetEmbeddingAsFile", "GetEmbeddingAsText", "GetTokenCount",
    "TextColor", "TextColorRemove", "TextFont", "TextFontRemove", "TextFormatRemove", "TextSize",
    "TextSizeRemove", "TextStyleAdd", "TextStyleRemove", "RGB",
    "JSONUniqueValues", "JSONRemoveValues", "JSONValueExists", "JSONValueCount", "JSONAnyValuesExist",
}
# Evaluate and EvaluationError are pure apart from field references in the text they evaluate.
TIER2_NOTES = {
    "Evaluate": "pure unless the evaluated text references fields; listed in tier 1 work as core",
    "EvaluationError": "pure unless the expression references fields",
    "IsValidExpression": "needs the schema to resolve field names in the text",
    "NPV": "takes a repeating field or related values as payments",
    "Self": "needs the object/field whose calculation is being evaluated",
}
CORE_TIER2 = {"Evaluate", "EvaluationError", "IsValidExpression", "GetField", "IsValid",
              "GetNthRecord", "List", "Count", "Sum", "Max", "Min", "Average", "ExecuteSQL",
              "GetRepetition", "Extend", "Last", "Self", "GetSummary"}

HOST_CAPS = {
    "GetContainerAttribute": ["container-data"], "GetHeight": ["container-data", "image-decoding"],
    "GetWidth": ["container-data", "image-decoding"], "GetThumbnail": ["container-data", "image-decoding"],
    "VerifyContainer": ["container-data", "external-storage"],
    "ReadQRCode": ["image-decoding", "os-vision"], "GetLiveText": ["os-vision"],
    "GetLiveTextAsJSON": ["os-vision"], "GetTextFromPDF": ["pdf"],
    "ComputeModel": ["coreml"], "GetModelAttributes": ["coreml"], "PredictFromModel": ["ml-model-store"],
    "GetEmbedding": ["network", "ai-provider"], "GetRAGSpaceInfo": ["network", "ai-provider"],
    "GetTableDDL": ["schema"], "GetFieldsOnLayout": ["layout", "schema"],
    "Furigana": ["japanese-ime-dictionary"],
    "GetLayoutObjectAttribute": ["layout", "window", "ui"], "GetLayoutObjectOwnerInfo": ["layout", "add-ons"],
    "LayoutObjectUUID": ["layout"], "WindowNames": ["window"],
    "GetAVPlayerAttribute": ["media-player", "device"], "GetSensor": ["device-sensors"],
    "Location": ["device-sensors", "location"], "LocationValues": ["device-sensors", "location"],
    "RangeBeacons": ["device-sensors", "bluetooth"],
    "ConvertFromFileMakerPath": ["filesystem-paths"], "ConvertToFileMakerPath": ["filesystem-paths"],
    "GetAddonInfo": ["add-ons"], "ExecuteSQL": ["records", "schema", "sql-engine"],
    "ExecuteSQLe": ["records", "schema", "sql-engine"], "GetNthRecord": ["records", "found-set"],
    "GetSummary": ["records", "found-set", "sort"], "Lookup": ["records", "relationships"],
    "LookupNext": ["records", "relationships", "index"], "GetField": ["records", "schema"],
    "IsValid": ["records", "schema"], "Self": ["calculation-owner"],
    "GetFieldName": ["schema"], "GetBaseTableName": ["schema"],
    "GetPersistentData": ["file-persistent-data"], "ListPersistentDataIDs": ["file-persistent-data"],
    "GetRecordIDsFromFoundSet": ["records", "found-set"], "Random": ["entropy"],
    "Evaluate": ["schema"], "EvaluationError": [], "IsValidExpression": ["schema"],
    "DayName": ["file-locale"], "MonthName": ["file-locale"], "SortValues": ["collation"],
    "UniqueValues": ["collation"], "Upper": ["file-locale"], "Lower": ["file-locale"],
    "Proper": ["file-locale"], "GetAsDate": ["file-locale"], "GetAsTime": ["file-locale"],
    "GetAsTimestamp": ["file-locale"], "GetAsNumber": ["file-locale"], "GetAsText": ["file-locale"],
    "CosineSimilarity": [], "GetTokenCount": ["tokenizer-cl100k"],
}
for _n in ("Average", "Count", "List", "Max", "Min", "StDev", "StDevP", "Sum", "Variance", "VarianceP"):
    HOST_CAPS.setdefault(_n, ["records", "relationships", "repetitions"])
for _n in ("Extend", "GetRepetition", "Last"):
    HOST_CAPS.setdefault(_n, ["repetitions"])
for _n in ("DatabaseNames", "FieldBounds", "FieldIDs", "FieldNames", "FieldRepetitions", "FieldStyle",
           "FieldType", "GetNextSerialValue", "LayoutIDs", "LayoutNames", "TableIDs", "TableNames",
           "RelationInfo", "ScriptIDs", "ScriptNames", "ValueListIDs", "ValueListNames", "ValueListItems",
           "FieldComment", "LayoutObjectNames", "BaseTableIDs", "BaseTableNames", "BaseTableComment",
           "FieldAnnotation", "FieldDisplayNames"):
    HOST_CAPS.setdefault(_n, ["schema", "open-files"])
HOST_CAPS["FieldBounds"] = ["schema", "layout"]
HOST_CAPS["FieldStyle"] = ["schema", "layout"]
HOST_CAPS["LayoutObjectNames"] = ["layout"]
HOST_CAPS["ValueListItems"] = ["schema", "records", "index"]
HOST_CAPS["GetNextSerialValue"] = ["schema", "records"]

# Engine implementation (Draco:: symbols). DBDataArg methods are found
# automatically from the opcode name; these are the exceptions.
IMPL_OVERRIDES = {
    "kDBIfOp": "DBCalcSolver::SolveIf",
    "kDBCaseOp": "DBCalcSolver::SolveCase (ConstructCase, PrepareForCase)",
    "kDBChooseOp": "DBCalcSolver::SolveChoose (ConstructChoose, PrepareForChoose)",
    "kDBLetOp": "DBCalcSolver::SolveLet (ConstructLet, PrepareForLet)",
    "kDBLoopOp": "DBCalcSolver::SolveLoop, SolveLoopIf, SolveLoopRewind (ConstructLoop1/2)",
    "kDBEvaluateOp": "DBCalcSolver::SolveEvaluate (ConstructEvalOrGetField)",
    "kDBGetFieldOp": "DBCalcSolver::SolveGetField (ConstructEvalOrGetField)",
    "kDBValidExprOp": "DBCalcSolver::SolveValidExpr",
    "kDBEvalErrorOp": "DBCalcSolver (kDBSolveEvalErrorOp 527)",
    "kDBIsValidOp": "DBCalcSolver (kDBSolveIsValidOp 528), DBDataArg::IsValid",
    "kDBLookupOp": "DBCalcSolver::SolveLookups (kDBSolveLookupOp 515)",
    "kDBLookupNextOp": "DBCalcSolver::SolveLookups (kDBSolveLookupNextOp 516)",
    "kDBSetPrecisionOp": "DBCalcSolver::SolveSetPrecision",
    "kDBSetRecursionOp": "DBCalcSolver::SolveSetRecursion",
    "kDBGetOp": "DBCalcSolver::SolveGetFunction -> host environment (virtual call)",
    "kDBAggregateListOp": "DBCalcSolver::SolveAggregates, DBDataArg::AggregateList",
    "kDBSortValuesOp": "DBDataArg::SortOrUniqueValues(sort)",
    "kDBUniqueValuesOp": "DBDataArg::SortOrUniqueValues(unique)",
    "kDBComputeModelOp": "DBDataArg::ComputeCoreMLModel",
    "kDBEmbeddedDataAsFileOp": "DBDataArg::EmbeddedDataAsFile",
    "kDBEmbeddedDataAsTextOp": "DBDataArg::EmbeddedDataAsText",
    "kDBNormalizeVectorOp": "DBDataArg::NormalizeVector",
    "kDBEmbeddingOp": "DBDataArg::GetEmbeddingVector",
    "kDBLLMPredictOp": "DBDataArg::PredictFromModel",
    "kDBVectorAddOp": "DBDataArg::VectorAdd (InternalVectorAddSub)",
    "kDBVectorSubOp": "DBDataArg::VectorSub (InternalVectorAddSub)",
    "kDBGenerateDDLOp": "DBDataArg::GenerateDDL",
    "kDBGenerateJSONLayoutSchemaOp": "DBDataArg::GenerateLLMJSONLayoutSchema",
    "kDBGetTokensCountOp": "DBDataArg::GetTokensCount",
    "kDBExtractTextFromPDFOp": "DBDataArg::ExtractTextFromPDF",
    "kDBGetRagSpaceInfoOp": "DBDataArg::GetRagSpaceInfo",
    "kDBGetModelAttrOp": "DBDataArg::GetModelAttributes",
    "kDBCryptEncryptB64Op": "DBDataArg::CryptEncryptB64",
    "kDBCryptDecryptB64Op": "DBDataArg::CryptDecryptB64",
    "kDBConvertFromFMPathOp": "DBDataArg::ConvertFromFMPath",
    "kDBConvertToFMPathOp": "DBDataArg::ConvertToFMPath",
    "kDBLayoutObjectUUIDOp": "DBDataArg::LayObjUUID",
    "kDBGetRecordIDsFromFoundSetVarOp": "DBDataArg::GetRecordIDsFromFoundSet",
    "kDBAVPlayerAttrOp": "DBDataArg::GetAVPlayerAttribute",
    "kDBJsonSetOp": "DBDataArg::JsonSet", "kDBJsonGetOp": "DBDataArg::JsonGet",
    "kDBJsonDeleteOp": "DBDataArg::JsonDelete", "kDBJsonKeyListOp": "DBDataArg::JsonKeyList",
    "kDBJsonValueListOp": "DBDataArg::JsonValueList", "kDBJsonFormatOp": "DBDataArg::JsonFormat",
    "kDBJsonGetElemTypeOp": "DBDataArg::JsonGetElemType", "kDBJsonMakeArrayOp": "DBDataArg::JsonMakeArray",
    "kDBJsonParseOp": "DBDataArg::JsonParse", "kDBJsonParsedStateOp": "DBDataArg::JsonParsedState",
    "kDBJsonUniqueValuesOp": "DBDataArg::JSONUniqueValues", "kDBJsonRemoveValuesOp": "DBDataArg::JSONRemoveValues",
    "kDBJsonValueExistsOp": "DBDataArg::JSONValueExists", "kDBJsonValueCountOp": "DBDataArg::JSONValueCount",
    "kDBJsonAnyValuesExistOp": "DBDataArg::JSONAnyValuesExist",
    "kDBGetContainerAttrOp": "DBDataArg::GetContainerAttribute",
    "kDBGetWidth": "DBDataArg::GetWidth", "kDBGetHeight": "DBDataArg::GetHeight",
    "kDBVerifyContainer": "DBDataArg::VerifyContainer", "kDBIndexItems": "DBDataArg::IndexItems",
    "kDBGetLayoutObjectAttribute": "DBDataArg::GetLayoutObjectAttribute",
    "kDBSubstituteOp": "DBDataArg::Substitute -> InternalSubstitute (kDBSolveSubstituteOp 512 for the list form)",
    "kDBFactorialOp": "DBDataArg::Factorial (kDBSolveFactorialOp 513)",
    "kDBTextFontOp": "DBDataArg::TextFont (kDBSolveTextFontOp 514)",
    "kDBTextColorRemoveOp": "DBDataArg::TextColorRemove (kDBSolveTextColorRemoveOp 529)",
    "kDBTextFontRemoveOp": "DBDataArg::TextFontRemove (kDBSolveTextFontRemoveOp 530)",
    "kDBTextSizeRemoveOp": "DBDataArg::TextSizeRemove (kDBSolveTextSizeRemoveOp 531)",
    "kDBTrimOp": "DBDataArg::Trim", "kDBATanOp": "DBDataArg::ATan", "kDBACosOp": "DBDataArg::ACos",
    "kDBASinOp": "DBDataArg::ASin", "kDBRgbOp": "DBDataArg::Rgb",
    "kDBSerialIncrementOp": "DBDataArg::SerialIncrement (IncrementSerialNumber)",
    "kDBNpvOp": "DBDataArg::Npv (NpvAggregator)",
    "kDBGetNthRecordOp": "DBDataArg::GetNthRecord", "kDBSelfOp": "DBDataArg::Self",
    "kDBExecuteSQLOp": "DBDataArg::ExecuteSQL (FQL::ExecuteSQLCalc)",
    "kDBExecuteSQLeOp": "DBDataArg::ExecuteSQLe (FQL::ExecuteSQLCalc)",
}
AGGREGATE_OPS = {"kDBAverageOp", "kDBCountOp", "kDBMaxOp", "kDBMinOp", "kDBStdevOp", "kDBStdevpOp",
                 "kDBSumOp", "kDBVarianceOp", "kDBVariancePOp"}

# Design functions are registered like plug-in functions by FMEngine
# (DesignFunctions.cpp: InstallDesignFunction, gDesignFunctions) and handled
# by one of these FMEngine routines (grouping inferred from the names).
DESIGN_IMPL = {
    "DatabaseNames": "DatabaseNames", "FieldBounds": "FieldLayoutInfo", "FieldRepetitions": "FieldLayoutInfo",
    "FieldStyle": "FieldLayoutInfo", "FieldIDs": "FieldIDnNames", "FieldNames": "FieldIDnNames",
    "FieldType": "FieldInfo", "FieldComment": "FieldInfo", "FieldAnnotation": "FieldInfo",
    "FieldDisplayNames": "FieldInfo", "GetNextSerialValue": "FieldInfo",
    "LayoutIDs": "LayoutIDnNames", "LayoutNames": "LayoutIDnNames", "TableIDs": "RelationIDnNames",
    "TableNames": "RelationIDnNames", "RelationInfo": "RelationInfo", "ScriptIDs": "ScriptIDnNames",
    "ScriptNames": "ScriptIDnNames", "ValueListIDs": "ValueListIDnNames", "ValueListNames": "ValueListIDnNames",
    "ValueListItems": "ValueListItems", "WindowNames": "WindowNames", "LayoutObjectNames": "LayoutObjectNames",
    "BaseTableIDs": "BaseIDnNames", "BaseTableNames": "BaseIDnNames", "BaseTableComment": "BaseTableInfo",
    "Location": "Location", "LocationValues": "LocationValues", "RangeBeacons": "RangeBeacons",
}

# Docs variable names that differ from the DWARF constant names.
DOCVAR_TO_OPCODE = {
    "kDBGetHeightOp": "kDBGetHeight", "kDBGetWidthOp": "kDBGetWidth",
    "kDBVerifyContainerOp": "kDBVerifyContainer", "kDBJsonGetTypeOp": "kDBJsonGetElemTypeOp",
    "kDBGeTokenCountOp": "kDBGetTokensCountOp", "kDBUTCmicroSecsOp": "kDBUTCMicroSecsOp",
    "KDBTriggerKeystrokeOp": "kDBTriggerKeystrokeOp", "kDBLastErrorDetailOp": "kDBLastExternalErrorDetailOp",
    "kDBLLMLastStepTokensUsedOp": "kDBLastStepTokensUsedOp",
    "kDBTransactionOpenStateOp": "kDBTransactionStateOp",
}

# ------------------------------------------------------------ Get selectors
GET_GROUPS = {
    "session": (2, ["session"], [
        "AccountExtendedPrivileges", "AccountGroupName", "AccountName", "AccountPasswordDaysRemaining",
        "AccountPrivilegeSetName", "AccountType", "CurrentExtendedPrivileges", "CurrentPrivilegeSetName",
        "UserName", "ApplicationVersion", "ApplicationLanguage", "ApplicationArchitecture", "SystemPlatform",
        "SystemVersion", "SystemLanguage", "HostName", "HostIPAddress", "HostApplicationVersion",
        "MultiUserState", "NetworkProtocol", "ConnectionState", "ConnectionAttributes", "UserCount",
        "SessionIdentifier", "PersistentID", "SystemIPAddress", "SystemNICAddress", "EncryptionState",
        "FileLocaleElements", "SystemLocaleElements", "UseSystemFormatsState", "CustomMenuSetName",
        "FileName", "FilePath", "FileSize", "OpenDataFileInfo", "DBEngineVersion", "StudioDatasource",
        "Device", "UUID", "UUIDNumber"]),
    "clock": (2, ["clock"], [
        "CurrentDate", "CurrentTime", "CurrentTimestamp", "CurrentHostTimestamp",
        "CurrentTimeUTCMilliseconds", "CurrentTimeUTCMicroseconds"]),
    "context": (2, ["records", "layout", "found-set"], [
        "FoundCount", "RecordNumber", "RecordID", "RecordModificationCount", "TotalRecordCount",
        "RecordOpenCount", "RecordOpenState", "RecordAccess", "LayoutAccess", "LayoutCount", "LayoutName",
        "LayoutNumber", "LayoutTableName", "LayoutViewState", "SortState", "RequestCount", "RequestOmitState",
        "WindowMode", "ModifiedFields", "CalculationRepetitionNumber", "ActiveRecordNumber", "QuickFindText"]),
    "script": (2, ["script-runtime"], [
        "ScriptName", "ScriptParameter", "ScriptResult", "LastError", "LastErrorDetail", "LastErrorLocation",
        "LastMessageChoice", "ErrorCaptureState", "AllowAbortState", "TransactionOpenState",
        "RevertTransactionOnErrorState", "ScriptAnimationState"]),
    "ui": (3, ["ui", "window"], [
        "ActiveFieldContents", "ActiveFieldName", "ActiveFieldTableName", "ActiveLayoutObjectName",
        "ActiveModifierKeys", "ActivePortalRowNumber", "ActiveRepetitionNumber", "ActiveSelectionSize",
        "ActiveSelectionStart", "AllowFormattingBarState", "HighContrastState", "SystemAppearance",
        "MenubarState", "StatusAreaState", "TextRulerVisible", "WindowContentHeight", "WindowContentWidth",
        "WindowDesktopHeight", "WindowDesktopWidth", "WindowHeight", "WindowLeft", "WindowName",
        "WindowOrientation", "WindowStyle", "WindowTop", "WindowUUID", "WindowVisible", "WindowWidth",
        "WindowZoomLevel", "ScreenDepth", "ScreenHeight", "ScreenWidth", "ScreenScaleFactor",
        "TriggerCurrentPanel", "TriggerTargetPanel", "TriggerGestureInfo", "TriggerKeystroke",
        "TriggerModifierKeys"]),
    "device": (3, ["device"], [
        "GuidedAccessState", "TouchKeyboardState", "RegionMonitorEvents", "NetworkType",
        "SystemStorageAvailable", "TriggerExternalEvent"]),
    "filesystem": (3, ["filesystem-paths"], [
        "DesktopPath", "DocumentsPath", "DocumentsPathListing", "FileMakerPath", "PreferencesPath",
        "TemporaryPath", "SystemDrive", "CacheFileName", "CacheFilePath"]),
    "plugins": (3, ["plugins"], ["InstalledFMPlugins", "InstalledFMPluginsAsJSON"]),
    "printing": (3, ["printer"], ["PrinterName", "PageNumber", "PageCount"]),
    "ai": (3, ["ai-provider"], ["LastStepTokensUsed"]),
}

# How a selector answers when evaluated by the FileMaker Script Engine
# (PSOS, schedules), the Data API/OData/CWP engines or WebDirect (all on the
# host). From the Claris pages unless marked "inferred".
GET_SERVER_NOTES = {
    "ApplicationVersion": "host engines answer 'Server <ver>' (FMSE/OData scripts), 'FileMaker Data API Engine <ver>', 'OData Engine <ver>', 'Web Publishing Engine <ver>' (CWP), 'Go <ver>'/'Go_iPad <ver>' on Go",
    "SystemIPAddress": "server-side: addresses of the database server machine, not the client",
    "SystemNICAddress": "server-side: NICs of the server machine; WebDirect: empty",
    "SystemPlatform": "server-side: platform of the host (1 macOS, -2 Windows, 5 CentOS, 8 Ubuntu); WebDirect: 4",
    "SystemVersion": "server-side: host OS version; WebDirect: '<os> <browser> <version>'",
    "Device": "server-side: type of the host computer (1 Mac, 2 Windows, 6 Linux)",
    "PersistentID": "server-side: id of the host machine; WebDirect: per browser session cookie",
    "UserName": "FMSE, Data API, CWP, WebDirect: the account name (no OS user)",
    "TemporaryPath": "server-side: a per-session folder deleted when the script ends; WebDirect/Data API/CWP: empty",
    "DocumentsPath": "server-side: the server's Documents folder; WebDirect/Data API/CWP: empty",
    "DocumentsPathListing": "WebDirect/Data API/CWP: empty",
    "DesktopPath": "only FileMaker Pro; empty elsewhere (including server-side)",
    "SystemDrive": "only FileMaker Pro; empty elsewhere",
    "FileMakerPath": "only FileMaker Pro and Go; empty elsewhere",
    "PreferencesPath": "only FileMaker Pro and Go; empty elsewhere",
    "CacheFileName": "server-side scripts, WebDirect, Data API, CWP: empty",
    "CacheFilePath": "server-side scripts, WebDirect, Data API, CWP: empty",
    "ScreenScaleFactor": "WebDirect, server-side scripts, Data API, CWP: 1",
    "ScriptResult": "PSOS with wait: the server script's result is returned to the calling client",
    "ErrorCaptureState": "server-side scripts always behave as if error capture were on",
    "HostApplicationVersion": "a FileMaker Pro host answers empty on the host itself",
    "ConnectionState": "intended for Pro/Go connections; WebDirect, Data API, CWP, server-side may have no connection to verify",
    "ActiveModifierKeys": "WebDirect and Go: 0",
    "WindowName": "server-side scripts have no window (inferred: empty)",
    "LayoutName": "server-side scripts start on the file's default layout (inferred from running-scripts-on-server)",
    "FoundCount": "server-side scripts start with their own found set (no client context)",
    "HighContrastState": "WebDirect and Linux hosts: empty",
    "SystemAppearance": "WebDirect and Linux hosts: empty",
    "ApplicationLanguage": "WebDirect: the browser's language",
    "SystemLanguage": "WebDirect: the browser's language",
    "SystemLocaleElements": "WebDirect: the host's locale; Misc.Active is false",
    "HostIPAddress": "not supported in FileMaker Cloud",
}

# Compatibility overrides: product -> value. Products: pro, go, webdirect,
# server (PSOS/schedules), data_api, cwp, cloud. "no" = not supported (the
# value returned is in the note).
COMPAT_OVERRIDES = {
    "TextColor": {"webdirect": "no"},
    "ComputeModel": {"pro": "partial", "webdirect": "partial", "server": "partial", "data_api": "partial",
                     "cwp": "partial", "cloud": "no", "_note": "iOS, iPadOS and macOS only (CoreML)"},
    "GetModelAttributes": {"pro": "partial", "webdirect": "partial", "server": "partial", "data_api": "partial",
                           "cwp": "partial", "cloud": "no", "_note": "iOS, iPadOS and macOS only (CoreML)"},
    "GetLiveText": {"pro": "partial", "go": "partial", "webdirect": "partial", "server": "partial",
                    "data_api": "partial", "cwp": "partial", "cloud": "no",
                    "_note": "macOS 15+/iOS 18+ Vision text recognition only"},
    "GetLiveTextAsJSON": {"pro": "partial", "go": "partial", "webdirect": "partial", "server": "partial",
                          "data_api": "partial", "cwp": "partial", "cloud": "no",
                          "_note": "macOS 15+/iOS 18+ Vision text recognition only"},
    "ReadQRCode": {"_note": "on Linux hosts from Ubuntu 22.04"},
    "GetSensor": {"pro": "no", "webdirect": "no", "server": "no", "data_api": "no", "cwp": "no", "cloud": "no",
                  "_note": "FileMaker Go only; empty when the sensor is unavailable"},
    "GetAVPlayerAttribute": {"pro": "no", "webdirect": "no", "server": "no", "data_api": "no", "cwp": "no",
                             "cloud": "no", "_note": "FileMaker Go media player"},
    "Location": {"pro": "no", "webdirect": "no", "server": "no", "data_api": "no", "cwp": "no", "cloud": "no",
                 "_note": "FileMaker Go only; FileMaker Pro returns empty"},
    "LocationValues": {"pro": "no", "webdirect": "no", "server": "no", "data_api": "no", "cwp": "no",
                       "cloud": "no", "_note": "FileMaker Go only; FileMaker Pro returns empty"},
    "RangeBeacons": {"pro": "no", "webdirect": "no", "server": "no", "data_api": "no", "cwp": "no",
                     "cloud": "no", "_note": "FileMaker Go only"},
    "Furigana": {"_note": "result depends on the platform's Japanese reading dictionary (inferred)"},
    "GetLayoutObjectAttribute": {"server": "partial", "data_api": "partial", "cwp": "partial",
                                 "_note": "needs a window and layout; WebDirect: web viewer content is empty"},
    "WindowNames": {"server": "partial", "data_api": "partial", "cwp": "partial",
                    "_note": "host engines have no user windows (inferred)"},
    "ValueListIDs": {"_note": "WebDirect: current file only"},
    "ValueListItems": {"_note": "WebDirect: current file only"},
    "ValueListNames": {"_note": "WebDirect: current file only"},
    "Evaluate": {"_note": "server-side Evaluate accepts English function names only"},
}

# Engine-derived per-function notes (short; details and evidence in calc-functions.md).
ENGINE_NOTES = {
    "Left": "counts grapheme clusters; count = integer part (truncated toward zero), used as unsigned 32-bit",
    "Right": "counts grapheme clusters from the end; count truncated toward zero, unsigned 32-bit",
    "Middle": "graphemes; start empty or <= 0 => 1; count >= remaining length (incl. negative, read unsigned) => rest of text",
    "Length": "grapheme count of text; byte size for container data",
    "Position": "case-insensitive (folding) search; result is a grapheme index",
    "PatternCount": "case-insensitive; non-overlapping (resumes after the match); empty searchString => 0",
    "Substitute": "case-sensitive literal search; keeps the styles of the surrounding text; list form applies pairs in order",
    "Exact": "compares by type; text compares characters only (styles ignored)",
    "Filter": "keeps graphemes of textToFilter that occur (exactly, case-sensitive) in filterText",
    "FilterValues": "values compared case-insensitively (folding)",
    "Trim": "removes U+0020 and characters >= U+00A0 whose Unicode category is space separator (e.g. U+3000, U+2000-U+200A); not tab, CR or U+00A0",
    "Upper": "locale-aware case mapping (file's text locale)",
    "Lower": "locale-aware case mapping (file's text locale)",
    "Proper": "TextBuffer::ProperCase with the file locale",
    "ValueCount": "separators: U+000A, U+000D, U+2028, U+2029",
    "Code": "more than 80 characters => ?  (kMaxTextSizeForCode = 80)",
    "Round": "precision = integer part of the argument; rounding by FixPt::Round",
    "Truncate": "precision = integer part of the argument",
    "Int": "truncates toward zero",
    "Div": "Divide then Floor: rounds toward negative infinity",
    "Random": "64-bit LCG, 53-bit result in [0,1) as a double, then converted to a decimal number",
    "Sin": "computed in IEEE double", "Cos": "computed in IEEE double", "Tan": "computed in IEEE double",
    "Atan": "computed in IEEE double", "Asin": "computed in IEEE double", "Acos": "computed in IEEE double",
    "Sqrt": "decimal Power(x; 0.5) with extra precision",
    "Pi": "decimal constant (FixPt::Pi), current precision",
    "Degrees": "x * 180 / Pi in decimal", "Radians": "x * Pi / 180 in decimal",
    "JSONFormatElements": "jsoncpp styled writer: tab indent, ' : ' separator, keys sorted, CR line ends (inferred)",
    "JSONGetElement": "jsoncpp strict reader (strict root, duplicate keys rejected); errors '? ' + message, newlines -> CR",
    "JSONSetElement": "jsoncpp document; numbers written by valueToString(double)",
    "WeekOfYearFiscal": "startingDay outside 1..7 => error",
    "DayName": "names from the file's locale",
    "MonthName": "names from the file's locale",
}

# Engine-derived or inferred test vectors: (function, expression, expected, source, note).
# "inferred" rows still need an oracle run before an implementation relies on them.
EXTRA_VECTORS = [
    ("PatternCount", 'PatternCount ( "aaaa" ; "aa" )', "2", "engine", "non-overlapping"),
    ("PatternCount", 'PatternCount ( "Abc abc ABC" ; "abc" )', "3", "engine", "case-insensitive"),
    ("PatternCount", 'PatternCount ( "abc" ; "" )', "0", "engine", "empty search string"),
    ("Middle", 'Middle ( "abcdef" ; 0 ; 2 )', "ab", "engine", "start <= 0 treated as 1"),
    ("Middle", 'Middle ( "abcdef" ; -5 ; 2 )', "ab", "engine", "start <= 0 treated as 1"),
    ("Middle", 'Middle ( "abcdef" ; 3 ; -1 )', "cdef", "inferred", "negative count read as unsigned => rest"),
    ("Middle", 'Middle ( "abcdef" ; 2.9 ; 2.9 )', "bc", "engine", "arguments truncated toward zero"),
    ("Left", 'Left ( "abcdef" ; 2.9 )', "ab", "engine", "truncation toward zero"),
    ("Left", 'Left ( "abcdef" ; -1 )', "abcdef", "inferred", "negative count read as unsigned 32-bit; confirm"),
    ("Left", 'Left ( "abc" ; 4294967296 )', "", "inferred", "count taken modulo 2^32; confirm"),
    ("Length", 'Length ( "e" & Char ( 769 ) )', "1", "engine", "grapheme cluster (e + combining acute)"),
    ("Length", 'Length ( Char ( 66560 ) )', "1", "engine", "U+10400 (surrogate pair) is one grapheme"),
    ("Position", 'Position ( "ABCabc" ; "c" ; 1 ; 1 )', "3", "engine", "case-insensitive"),
    ("Substitute", 'Substitute ( "aAa" ; "a" ; "x" )', "xAx", "engine", "case-sensitive"),
    ("Exact", 'Exact ( "a" ; "A" )', "0", "engine", "case-sensitive"),
    ("Filter", 'Filter ( "AbCabc" ; "abc" )', "babc", "engine", "case-sensitive grapheme match"),
    ("Trim", 'Code ( Trim ( Char ( 12288 ) & "x" & Char ( 12288 ) ) )', "120", "engine", "U+3000 is trimmed"),
    ("Trim", 'Length ( Trim ( Char ( 160 ) & "x" ) )', "2", "inferred", "U+00A0 (category 11) is not trimmed; confirm"),
    ("Trim", 'Length ( Trim ( Char ( 9 ) & "x" ) )', "2", "engine", "tab is not trimmed"),
    ("ValueCount", 'ValueCount ( "a" & Char ( 10 ) & "b" )', "2", "engine", "LF separates values"),
    ("ValueCount", 'ValueCount ( "a" & Char ( 8232 ) & "b" & Char ( 8233 ) & "c" )', "3", "engine", "U+2028/U+2029 separate values"),
    ("ValueCount", 'ValueCount ( "a" & Char ( 133 ) & "b" )', "1", "engine", "NEL is not a separator"),
    ("ValueCount", 'ValueCount ( "a" & Char ( 13 ) & Char ( 10 ) & "b" )', "3", "inferred", "CR LF counts as two separators; confirm"),
    ("Int", "Int ( -2.7 )", "-2", "engine", "toward zero"),
    ("Div", "Div ( -7 ; 2 )", "-4", "engine", "floor"),
    ("WeekOfYearFiscal", 'WeekOfYearFiscal ( Date ( 1 ; 1 ; 2024 ) ; 8 )', "?", "inferred", "startingDay outside 1..7; result form to confirm"),
    ("JSONGetElement", "JSONGetElement ( \"{\\\"a\\\":1,\\\"a\\\":2}\" ; \"a\" )", "? * Line 1, Column 9\r  Duplicate key: 'a'", "inferred", "jsoncpp rejectDupKeys; exact column and trailing text to confirm"),
    ("JSONGetElement", 'JSONGetElement ( "1" ; "" )', "? * Line 1, Column 1\r  A valid JSON document must be either an array or an object value.", "inferred", "jsoncpp strictRoot; exact text to confirm"),
    ("JSONSetElement", 'JSONSetElement ( "{}" ; "b" ; 1 ; JSONNumber ) ', '{"b":1}', "inferred", "compact writer"),
    ("JSONSetElement", 'JSONSetElement ( "{\\"b\\":1}" ; "a" ; "x" ; JSONString )', '{"a":"x","b":1}', "inferred", "keys sorted"),
    ("JSONGetElementType", 'JSONGetElementType ( "{\\"a\\":[]}" ; "a" )', "4", "engine", "kJSONArrayValue = 4"),
]
