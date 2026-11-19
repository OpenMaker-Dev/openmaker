// Research harness, see README.md. Runs only inside the network-less amd64
// container. Runs FileMaker scripts with FileMaker Server 26's own script
// engine (libFMEngine.so on libDBEngine.so/libSupport.so, called through their
// exported entry points; no FileMaker code is copied) the way the server's
// script engine process (fmsased, FMScriptThread::dispatchCallBack) runs a
// schedule: a new server session (Draco::SMSession) per run and
// SMSession::RunScriptAndClose(file, account, script name, parameter, &result),
// which opens the file, opens its window, runs the script to completion with
// SMScriptRuntime::ExecuteScript and closes the file again.
//
// Input lines (stdin, TAB-separated, escapes as calceval: \n \r \t \\ \uXXXX):
//   script name <TAB> parameter <TAB> expression <TAB> expression ...
// The expressions are evaluated after the script has finished, before the
// file is closed, in the same session (so $$ variables and Get(LastError)
// are the script's). Output per run:
//   name <TAB> parameter <TAB> error <TAB> result <TAB> lastError <TAB> value ...
// followed by the step trace, one line per executed step:
//   > <TAB> depth <TAB> script <TAB> line <TAB> step id <TAB> step error
//     <TAB> handled <TAB> control <TAB> LastError after <TAB> watch values ...
// and the event-log lines the server would write (SMSession::LogOrCountError):
//   ! <TAB> code <TAB> message
// and the scripts started by "@call":
//   @ <TAB> call <target> state <n> bottom <b> -> error <e> <TAB> depth <d>
// Directives: "@watch <TAB> expr ..." (expressions evaluated after every
// step and printed on its trace line; "@watch" alone clears them),
// "@trace off|on", "@account <name> <TAB> <password>" (log in with an account
// instead of the security bypass FMDeveloperTool uses), "@context <hex>"
// (table occurrence for the expressions, stored form as in calceval),
// "@call <TAB> script <TAB> line <TAB> target <TAB> state <TAB> bottom"
// (start target through CallScript after that step), "@scan <TAB> name ..."
// (write the scripts' cached block indexes with FileMaker's ScanScript),
// "@path <path>" (run the following lines on another file path, for a file
// that cannot be opened; "@path" alone goes back to the file given).
// "-l" lists the file's script names instead (SMSession::GetScriptNames).
// The trace comes from interposed libFMEngine functions (below); STEP_LIMIT
// bounds the steps of one run. SASE_EXPORT runs Export Records as fmsased
// does (the DispatchStep and file filter hooks below).
#include <cstdio>
#include <cstring>
#include <cstdlib>
#include <string>
#include <vector>
#include <memory>
#include <iostream>
#include <sstream>
#include <execinfo.h>
#include <signal.h>
#include <unistd.h>
#include <dlfcn.h>
static void crashed(int sig) {
    void* frames[64];
    int n = backtrace(frames, 64);
    fprintf(stderr, "signal %d\n", sig);
    backtrace_symbols_fd(frames, n, 2);
    _exit(3);
}

namespace Draco {
class unistring : public std::u16string {
public:
    unistring() {}
    unistring(const std::u16string& s) : std::u16string(s) {}
};
struct UniversalPath {
    unistring a, b, c;
    bool SetCommandLineFormat(unistring const&);
};
template <unsigned char N> class HBSerialKey { public: unsigned char b[N + 3]; };
class DBFileUser;
class DBCursor;
class DBAuthenticationInfo {
public:
    enum AuthTypes : int {};
    alignas(16) char b[0x1000];
    DBAuthenticationInfo(AuthTypes);
    ~DBAuthenticationInfo();
    void SetToUserNamePassword(unistring const&, unistring const&, bool const&);
};
class HBEncryptionInfo;
class DBEngineController { public: alignas(16) char b[0x10000]; DBEngineController(); void Start(bool, unsigned int, HBEncryptionInfo*, bool, bool, bool); };
class ApplicationInfo { public: enum AppType : int {}; static ApplicationInfo* GetInstance(); void SetApplicationInfo(unistring const&, unistring const&, AppType); };
class Text { public: alignas(16) char b[0x400]; Text(); Text(char16_t const*, unsigned int); ~Text(); };
class TextBuffer { public: void Get(unistring&, unsigned int, unsigned int) const; };
class DBData { public: alignas(16) char b[0x400]; DBData(); ~DBData(); Text const& GetAsText() const; };
// DBError is {short code; ushort extended-error index}, returned in memory
// (non-trivial copy and destructor).
class DBError { public: short code; unsigned short index; DBError() : code(0), index(0) {} DBError(const DBError& o) : code(o.code), index(o.index) {} ~DBError() {} };
class DBExpressionEnv {
public:
    alignas(16) char b[0x100];
    void Setup(DBFileUser const*, HBSerialKey<5> const&, DBCursor const*, DBCursor const*);
    int Evaluate(Text const&, DBData&) const;
};
// The server session (libFMEngine, DWARF size 0xd38): FMSession with the
// server's script runtime (SMScriptRuntime) and file users (SMFileUser).
class SMSession {
public:
    alignas(16) char b[0x2000];
    SMSession(bool callLogMessage, DBAuthenticationInfo const&);
    DBError RunScriptAndClose(UniversalPath const&, DBAuthenticationInfo&, unistring const& script, unistring const& parameter, unistring* result);
    // static: opens the file in a session of its own
    static DBError GetScriptNames(UniversalPath const&, DBAuthenticationInfo&, std::vector<unistring>&, unistring const&);
};
class ScriptStackElement;
class ScriptStackElementScript { public: void GetName(unistring&) const; };
class FMFileUser;
class FMWindowModel;
class ScriptRuntimeBase {
public:
    ScriptStackElement* GetCurrentStackFrame() const;
    FMFileUser const& GetFileUser() const;
    DBData* GetScriptResult() const;
    FMWindowModel* GetWindow(bool) const;
};
// Export Records' handler (libFMEngine). fmsased's
// FMS::SASEScriptRuntime::DispatchStep (fmsased 0x79bb20) runs step 36 with
// one on the script's window and returns its error.
class FMExportManager {
public:
    alignas(16) char b[0x4000];  // fmsased keeps it in a 0xfc8-byte frame
    FMExportManager();
    ~FMExportManager();
    DBError PerformExport(FMWindowModel&);
};
void InstallDesignFunction();
// Opening a file in a session and editing a script (the steps of
// ObjectTransferScriptSteps::CallDeserializeFromXML, FileMaker's XML script
// importer, with ScanScript in place of the XML reader).
class FMCertVerifyCallBackInfo;
class DBSession {
public:
    DBError OpenFileUser(std::shared_ptr<DBFileUser>&, std::vector<UniversalPath> const&, DBFileUser const*,
                         DBAuthenticationInfo const&, unsigned int, FMCertVerifyCallBackInfo*);
};
class FMSession { public: DBError CloseAllFileUsers(); };
class HBAbstractKey;
class InteractiveErrorHandler;
class RefCountedObj { public: void AddReference(); };
class ScriptStepList;  // DBCatalogItemList; mStepList (vector of 0x44-byte ScriptStep) at +0x278
// FMScriptKey (HBSerialKey<5>) as returned by value: in memory (non-trivial)
struct ScriptKey { unsigned char b[16]; ScriptKey() { memset(b, 0, sizeof b); } ScriptKey(const ScriptKey& o) { memcpy(b, o.b, sizeof b); } ~ScriptKey() {} };
class SMScriptRuntime { public: static ScriptKey ConvertScriptNameToKey(FMFileUser const&, unistring const&); };
class FMScriptCatalog { public: alignas(16) char b[0x1000]; FMScriptCatalog(DBFileUser const&); };
class FMScript {
public:
    alignas(16) char b[0x1000];  // DWARF size 0x598; mSteps (ScriptStepList) at +0x2e8
    FMScript(FMScriptCatalog&, HBAbstractKey const&);
    DBError Open(bool, bool);
    DBError LoadScript(ScriptStepList&) const;
    void SaveScript() const;
};
class DBCatalogMember { public: DBError Commit(bool, InteractiveErrorHandler*); };
struct ScriptStepListIter { ScriptStepList* list; short index; };  // DBCatalogItemListPtr, mStepIndex
}  // namespace Draco
// ScriptUtility.cpp: computes the cached block indexes of a step list (the
// step values of If/Else/Loop/..., the End If and End Loop numeric data),
// what the Script Workspace stores; nothing in the server build calls it.
Draco::DBError ScanScript(Draco::ScriptStepListIter const&);

using namespace Draco;

static std::u16string from_utf8(const std::string& s) {
    std::u16string out;
    for (size_t i = 0; i < s.size();) {
        unsigned c = (unsigned char)s[i];
        unsigned cp; int n;
        if (c < 0x80) { cp = c; n = 1; }
        else if ((c >> 5) == 6) { cp = c & 0x1f; n = 2; }
        else if ((c >> 4) == 14) { cp = c & 0x0f; n = 3; }
        else { cp = c & 0x07; n = 4; }
        for (int k = 1; k < n && i + k < s.size(); k++) cp = (cp << 6) | ((unsigned char)s[i + k] & 0x3f);
        i += n;
        if (cp >= 0x10000) { cp -= 0x10000; out.push_back(0xD800 + (cp >> 10)); out.push_back(0xDC00 + (cp & 0x3ff)); }
        else out.push_back((char16_t)cp);
    }
    return out;
}
static std::string to_utf8_escaped(const std::u16string& s) {
    std::string out;
    for (size_t i = 0; i < s.size(); i++) {
        unsigned cp = s[i];
        if (cp >= 0xD800 && cp < 0xDC00 && i + 1 < s.size()) { cp = 0x10000 + ((cp - 0xD800) << 10) + (s[i + 1] - 0xDC00); i++; }
        if (cp == '\\') { out += "\\\\"; continue; }
        if (cp == '\r') { out += "\\r"; continue; }
        if (cp == '\n') { out += "\\n"; continue; }
        if (cp == '\t') { out += "\\t"; continue; }
        if (cp < 0x20 || (cp >= 0xE000 && cp < 0xF900)) { char buf[16]; snprintf(buf, sizeof buf, "\\u%04X", cp); out += buf; continue; }
        if (cp < 0x80) out += (char)cp;
        else if (cp < 0x800) { out += (char)(0xC0 | (cp >> 6)); out += (char)(0x80 | (cp & 0x3f)); }
        else if (cp < 0x10000) { out += (char)(0xE0 | (cp >> 12)); out += (char)(0x80 | ((cp >> 6) & 0x3f)); out += (char)(0x80 | (cp & 0x3f)); }
        else { out += (char)(0xF0 | (cp >> 18)); out += (char)(0x80 | ((cp >> 12) & 0x3f)); out += (char)(0x80 | ((cp >> 6) & 0x3f)); out += (char)(0x80 | (cp & 0x3f)); }
    }
    return out;
}
static std::string unescape(const std::string& s) {
    std::string out;
    for (size_t i = 0; i < s.size(); i++) {
        if (s[i] == '\\' && i + 1 < s.size()) {
            char n = s[++i];
            unsigned cp;
            if (n == 'n') out += '\n'; else if (n == 'r') out += '\r'; else if (n == 't') out += '\t'; else if (n == '\\') out += '\\';
            else if (n == 'u' && i + 4 < s.size() && sscanf(s.c_str() + i + 1, "%4x", &cp) == 1) {
                if (cp < 0x80) out += (char)cp;
                else if (cp < 0x800) { out += (char)(0xC0 | (cp >> 6)); out += (char)(0x80 | (cp & 0x3f)); }
                else { out += (char)(0xE0 | (cp >> 12)); out += (char)(0x80 | ((cp >> 6) & 0x3f)); out += (char)(0x80 | (cp & 0x3f)); }
                i += 4;
            } else { out += '\\'; out += n; }
        } else out += s[i];
    }
    return out;
}
static std::vector<std::string> split_tabs(const std::string& s) {
    std::vector<std::string> f;
    size_t a = 0, b;
    while ((b = s.find('\t', a)) != std::string::npos) { f.push_back(s.substr(a, b - a)); a = b + 1; }
    f.push_back(s.substr(a));
    return f;
}
static std::string unhex(const std::string& h) {
    std::string out;
    for (size_t i = 0; i + 1 < h.size(); i += 2) { unsigned v; sscanf(h.c_str() + i, "%2x", &v); out += (char)v; }
    return out;
}
static HBSerialKey<5> parse_key(const std::string& h) {
    HBSerialKey<5> k; memset(k.b, 0, sizeof k.b);
    std::string b = unhex(h);
    if (b.size() > 5) b.resize(5);
    k.b[0] = (unsigned char)b.size();
    memcpy(k.b + 1, b.data(), b.size());
    return k;
}
static std::u16string text_of(const Text& t) {
    unistring u;
    ((const TextBuffer*)((const char*)&t + 0x130))->Get(u, 0, 0x7fffffff);
    return u;
}

// Runtime state read by the trace (DWARF of ScriptRuntimeBase,
// ScriptRuntimeModel at +0x3b0, ScriptStackElementScript):
//   +0x3b9 mDataModel.mLastError (DBError, code first)
//   +0x458/+0x460 mDataModel.mScriptStack (vector of frame pointers)
//   +0x504 mCurrentScriptStep.mStepID
//   frame +0x5f8: the script frame's current index (mScriptStepListIter + 8)
// ScriptStackElement vtable slot 0x60 is IsScript().
static short last_error(const void* rt) { short c; memcpy(&c, (const char*)rt + 0x3b9, 2); return c; }
static int stack_depth(const void* rt) {
    char* const* v = (char* const*)((const char*)rt + 0x458);
    return (int)((v[1] - v[0]) / 8);
}
static bool frame_is_script(ScriptStackElement* f) {
    typedef bool (*IsScriptFn)(const ScriptStackElement*);
    return f && ((IsScriptFn)(*(void***)f)[0x60 / 8])(f);
}

static std::vector<std::u16string> g_watch;
static bool g_trace = true;
static HBSerialKey<5> g_context;
static std::vector<std::string> g_trace_lines;
static std::vector<std::u16string> g_post;  // expressions evaluated after the script
static std::vector<std::string> g_post_values;
// The file user the running script was started on (ExecuteScript's), which
// "@watch" evaluates in: ScriptRuntimeBase::GetFileUser() faults in this
// session.
static const DBFileUser* g_file_user;

static std::string evaluate(const DBFileUser* fu, const std::u16string& ex) {
    DBExpressionEnv* env = (DBExpressionEnv*)calloc(1, sizeof(DBExpressionEnv));
    env->Setup(fu, g_context, nullptr, nullptr);
    Text t(ex.data(), (unsigned)ex.size());
    DBData d;
    int err = env->Evaluate(t, d);
    std::string v = to_utf8_escaped(text_of(d.GetAsText()));
    free(env);  // leaks the env's members on purpose; Setup holds no resources we need back
    return err ? "#" + std::to_string(err) + " " + v : v;
}

// The step being executed, recorded when Execute() starts (the frame may be
// gone by PostStepCleanup, e.g. after Exit Script or Halt Script). A stack,
// in case a step runs another script to completion inside Execute().
struct StepCtx { int depth; std::string script; int line; short id; };
static std::vector<StepCtx> g_steps;
static long g_step_count, g_step_limit = 20000;  // per run; STEP_LIMIT overrides
// "@call": after step <line> of <script> has run, start <target> the way a
// button, trigger or callback does, with ScriptRuntimeBase::CallScript's
// newState (the "current script" option: 0 continue, 1 halt, 2 exit,
// 3 resume, 4 pause, 5 suspend and resume) and queueAtBottom.
struct CallSpec { std::string script; int line; std::string target; unsigned state; bool bottom; };
static std::vector<CallSpec> g_calls;
static void call_script(void* rt, const CallSpec& c) {
    typedef DBError* (*Fn)(DBError*, void*, FMWindowModel*, const void*, void**, unsigned, bool, void*, void**, void*, DBError*);
    static Fn call = (Fn)dlsym(RTLD_DEFAULT, "_ZN5Draco17ScriptRuntimeBase10CallScriptERNS_13FMWindowModelERKNS_11HBSerialKeyILh5EEESt10unique_ptrINS_6DBDataENS8_18unique_ptr_deleterEEtbPNS_12TriggerStateEPPNS_24ScriptStackElementScriptEPS8_PNS_7DBErrorE");
    ScriptRuntimeBase* r = (ScriptRuntimeBase*)rt;
    FMWindowModel* w = r->GetWindow(false);
    ScriptKey k = SMScriptRuntime::ConvertScriptNameToKey(r->GetFileUser(), unistring(from_utf8(c.target)));
    void* param = nullptr;  // the scriptParameter unique_ptr, passed by invisible reference
    DBError e;
    if (w) call(&e, rt, w, k.b, &param, c.state, c.bottom, nullptr, nullptr, nullptr, nullptr);
    g_trace_lines.push_back("@\tcall " + c.target + " state " + std::to_string(c.state) + " bottom " + std::to_string(c.bottom) +
                            (w ? " -> error " + std::to_string(e.code) : " -> no window") + "\tdepth " + std::to_string(stack_depth(rt)));
}
static std::string g_run_head;                  // the run line, printed if the limit stops us

// Interposed libFMEngine functions. Only these symbols are exported from the
// executable (run.sh), so they take the place of libFMEngine's definitions
// in its PLT and vtable relocations; each one calls the original found with
// RTLD_NEXT.
extern "C" {
// ScriptRuntimeBase::Execute(): DoNextStep's call for every enabled step,
// after the step descriptor was copied into mCurrentScriptStep.
void _ZN5Draco17ScriptRuntimeBase7ExecuteEv(void* rt) {
    typedef void (*Fn)(void*);
    static Fn orig = (Fn)dlsym(RTLD_NEXT, "_ZN5Draco17ScriptRuntimeBase7ExecuteEv");
    StepCtx c{stack_depth(rt), "-", 0, 0};
    ScriptStackElement* f = ((ScriptRuntimeBase*)rt)->GetCurrentStackFrame();
    if (frame_is_script(f)) {
        unistring n;
        ((ScriptStackElementScript*)f)->GetName(n);
        c.script = to_utf8_escaped(n);
        short idx; memcpy(&idx, (char*)f + 0x5f8, 2);
        c.line = idx + 1;
    }
    memcpy(&c.id, (char*)rt + 0x504, 2);
    if (++g_step_count > g_step_limit) {
        // Runaway script: report what ran and stop the whole process (the
        // engine offers no clean way out of a running server script).
        std::cout << g_run_head << "\tstep limit " << g_step_limit << "\n";
        size_t n = g_trace_lines.size();
        for (size_t i = 0; i < n && i < 200; i++) std::cout << g_trace_lines[i] << "\n";
        if (n > 200) std::cout << ">\t... " << n - 200 << " more\n";
        std::cout.flush();
        _exit(4);
    }
    g_steps.push_back(c);
    orig(rt);
    g_steps.pop_back();
    for (auto& k : g_calls)
        if (k.script == c.script && k.line == c.line) call_script(rt, k);
}
// ScriptRuntimeBase::PostStepCleanup(DBError& stepError, bool handled, bool
// controlStep): called by Execute() once per executed step, with the error
// the step returned.
void _ZN5Draco17ScriptRuntimeBase15PostStepCleanupERNS_7DBErrorEbb(void* rt, DBError* err, bool handled, bool control) {
    typedef void (*Fn)(void*, DBError*, bool, bool);
    static Fn orig = (Fn)dlsym(RTLD_NEXT, "_ZN5Draco17ScriptRuntimeBase15PostStepCleanupERNS_7DBErrorEbb");
    short stepErr = err->code;
    orig(rt, err, handled, control);
    if (!g_trace) return;
    StepCtx c = g_steps.empty() ? StepCtx{stack_depth(rt), "?", 0, 0} : g_steps.back();
    std::string l = ">\t" + std::to_string(c.depth) + "\t" + c.script + "\t" + std::to_string(c.line) + "\t" + std::to_string(c.id) + "\t" +
                    std::to_string(stepErr) + "\t" + std::to_string(handled) + "\t" + std::to_string(control) + "\t" +
                    std::to_string(last_error(rt));
    if (!g_watch.empty() && g_file_user) {
        for (auto& w : g_watch) l += "\t" + evaluate(g_file_user, w);
    }
    g_trace_lines.push_back(l);
}
// SMScriptRuntime::ExecuteScript(SMFileUser&, name, parameter, result*):
// RunScriptAndClose's call (runtime vtable slot 0x288). Evaluates the
// post-run expressions after the script has finished.
DBError* _ZN5Draco15SMScriptRuntime13ExecuteScriptERNS_10SMFileUserERKNS_9unistringES5_PS3_(DBError* ret, void* rt, void* fu, const unistring* name,
                                                                                              const unistring* param, unistring* result) {
    typedef DBError* (*Fn)(DBError*, void*, void*, const unistring*, const unistring*, unistring*);
    static Fn orig = (Fn)dlsym(RTLD_NEXT, "_ZN5Draco15SMScriptRuntime13ExecuteScriptERNS_10SMFileUserERKNS_9unistringES5_PS3_");
    g_file_user = (const DBFileUser*)fu;
    orig(ret, rt, fu, name, param, result);
    g_file_user = nullptr;
    g_post_values.clear();
    g_post_values.push_back(std::to_string(last_error(rt)));
    for (auto& e : g_post) g_post_values.push_back(evaluate((const DBFileUser*)fu, e));
    return ret;
}
// ScriptRuntimeBase::AllowFileTriggers(FMFileUser*, bool): runtime vtable
// slot 0xb8. fmsased's FMS::SASEScriptRuntime overrides it to return the
// master trigger switch (ScriptRuntimeModel::mMasterTriggerSwitch, runtime
// +0x43c) alone; the base asks AllowTriggers, which also wants the
// debugger's user-trigger switch or a running step, so without this file
// triggers (OnFirstWindowOpen, OnLastWindowClose) would not fire as they do
// in fmsased. FILE_TRIGGERS=base keeps libFMEngine's behavior.
bool _ZN5Draco17ScriptRuntimeBase17AllowFileTriggersEPNS_10FMFileUserEb(void* rt, void* fu, bool b) {
    typedef bool (*Fn)(void*, void*, bool);
    static Fn orig = (Fn)dlsym(RTLD_NEXT, "_ZN5Draco17ScriptRuntimeBase17AllowFileTriggersEPNS_10FMFileUserEb");
    static bool base = getenv("FILE_TRIGGERS") && !strcmp(getenv("FILE_TRIGGERS"), "base");
    if (base) return orig(rt, fu, b);
    return *((const char*)rt + 0x43c) != 0;
}
// SMScriptRuntime::DispatchStep(bool&): runtime vtable slot (relocation at
// libFMEngine 0x12a3560). With SASE_EXPORT set, Export Records (36) runs as
// fmsased's SASEScriptRuntime::DispatchStep runs it: an FMExportManager's
// PerformExport on GetWindow(true), whose error is the step's. Every other
// step, and 36 without SASE_EXPORT, goes to libFMEngine's (error 3 for 36).
DBError* _ZN5Draco15SMScriptRuntime12DispatchStepERb(DBError* ret, void* rt, bool* b) {
    typedef DBError* (*Fn)(DBError*, void*, bool*);
    static Fn orig = (Fn)dlsym(RTLD_NEXT, "_ZN5Draco15SMScriptRuntime12DispatchStepERb");
    typedef void (*SetIndex)(DBError*, unsigned short);
    static SetIndex set_index = (SetIndex)dlsym(RTLD_DEFAULT, "_ZN5Draco5Error21SetErrorIndexInternalEt");
    static bool sase = getenv("SASE_EXPORT") != nullptr;
    short id; memcpy(&id, (char*)rt + 0x504, 2);
    if (!sase || (id != 36 && id != -36)) return orig(ret, rt, b);
    FMWindowModel* w = ((ScriptRuntimeBase*)rt)->GetWindow(true);
    FMExportManager* m = new FMExportManager();
    DBError e = m->PerformExport(*w);
    delete m;
    // ret is uninitialized: copy the error with its extended-error index
    // reference (Error::SetErrorIndexInternal), then release the local's.
    ret->code = e.code;
    ret->index = 0;
    if (e.index) { set_index(ret, e.index); set_index(&e, 0); }
    return ret;
}
// DBFileUser::GetDefaultFileFilter() const: the filter of paths a script may
// write (SMFileUser's vtable, relocation at libFMEngine 0x12a3300).
// libDBEngine's returns AlwaysFail, so every export path is error 800;
// fmsased's SASEFileUser returns SessionTempAndDocDirectoryFileFilter (the
// session's temporary folder and the server's Documents folder). With
// SASE_EXPORT the harness accepts every path in its container instead.
typedef bool (*FileFilter)(const UniversalPath&, void*, bool);
static bool accept_path(const UniversalPath&, void*, bool) { return true; }
FileFilter _ZNK5Draco10DBFileUser20GetDefaultFileFilterEv(const void* fu) {
    typedef FileFilter (*Fn)(const void*);
    static Fn orig = (Fn)dlsym(RTLD_NEXT, "_ZNK5Draco10DBFileUser20GetDefaultFileFilterEv");
    return getenv("SASE_EXPORT") ? accept_path : orig(fu);
}
// SMSession::LogOrCountError(code, message): the server's event-log report
// of a script error (SMScriptRuntime::CheckError).
void _ZN5Draco9SMSession15LogOrCountErrorEiRKNS_9unistringE(void* s, int code, const unistring* msg) {
    typedef void (*Fn)(void*, int, const unistring*);
    static Fn orig = (Fn)dlsym(RTLD_NEXT, "_ZN5Draco9SMSession15LogOrCountErrorEiRKNS_9unistringE");
    if (g_trace) g_trace_lines.push_back("!\t" + std::to_string(code) + "\t" + to_utf8_escaped(*msg));
    orig(s, code, msg);
}
}

// "@scan <TAB> name ...": write the cached block indexes of these scripts
// with FileMaker's ScanScript (FMUpgradeTool's XML importer leaves them 0).
static void scan_scripts(DBAuthenticationInfo* auth, const char* file, const std::vector<std::string>& names) {
    SMSession* s = new SMSession(false, *auth);
    std::vector<UniversalPath> paths(1);
    paths[0].SetCommandLineFormat(unistring(from_utf8(file)));
    std::shared_ptr<DBFileUser> fu;
    DBError e = ((DBSession*)s)->OpenFileUser(fu, paths, nullptr, *auth, 0, nullptr);
    if (!fu) { std::cout << "# scan: open error " << e.code << "\n"; return; }
    FMScriptCatalog* cat = new FMScriptCatalog(*fu);
    for (auto& n : names) {
        ScriptKey k = SMScriptRuntime::ConvertScriptNameToKey(*(const FMFileUser*)fu.get(), unistring(from_utf8(n)));
        FMScript* sc = new FMScript(*cat, *(const HBAbstractKey*)k.b);
        ScriptStepList* list = (ScriptStepList*)(sc->b + 0x2e8);
        DBError oe = sc->Open(false, false);
        DBError le = sc->LoadScript(*list);
        ((RefCountedObj*)list)->AddReference();  // the iterator below holds it; never released
        ScriptStepListIter it{list, 0};
        DBError se = ScanScript(it);
        // Read back what ScanScript stored: step id and step value (a 0-based
        // step index for block steps) per step, and check that every block
        // step names a partner of the right kind (script-steps.md 3.1).
        std::vector<std::pair<short, unsigned short>> st;
        char* const* v = (char* const*)((char*)list + 0x278);
        for (char* p = v[0]; p < v[1]; p += 0x44) {
            short id; unsigned short value;
            memcpy(&id, p + 0x14, 2); memcpy(&value, p + 0x16, 2);
            st.push_back({id, value});
        }
        std::string values, bad;
        for (size_t i = 0; i < st.size(); i++) {
            values += " " + std::to_string(i) + ":" + std::to_string(st[i].first) + "=" + std::to_string(st[i].second);
            short id = st[i].first, to = st[i].second < st.size() ? st[st[i].second].first : 0;
            bool ok = true;
            if (id == 68 || id == 125) ok = to == 125 || to == 69 || to == 70;
            else if (id == 69) ok = to == 70;
            else if (id == 71 || id == 72) ok = to == 73;
            else if (id == 73) ok = to == 71;
            else if (id == 205) ok = to == 206;
            if (!ok) bad += " " + std::to_string(i);
        }
        sc->SaveScript();
        DBError ce = ((DBCatalogMember*)sc)->Commit(true, nullptr);
        std::cout << "# scan " << n << ": open " << oe.code << " load " << le.code << " scan " << se.code << " commit " << ce.code
                  << "; index:id=value (0-based):" << values << "\n";
        if (se.code)  // e.g. 3702 for a Commit Transaction without Open: unbalanced on purpose
            std::cout << "# scan warning: ScanScript error " << se.code << " for " << n << " (unbalanced blocks)\n";
        if (oe.code || le.code || ce.code || st.empty() || (!se.code && !bad.empty())) {
            // A probe script without correct block indexes would loop or jump
            // wrongly (an unscanned End Loop holds 0); stop instead.
            std::cout << "# scan FAILED for " << n << (st.empty() ? " (script not found or empty)" : "")
                      << (bad.empty() ? "" : " (wrong partner at index" + bad + ")") << "\n";
            std::cout.flush();
            _exit(5);
        }
    }
    DBError ce = ((FMSession*)s)->CloseAllFileUsers();
    std::cout << "# scan: close " << ce.code << "\n";
    std::cout.flush();
}

int main(int argc, char** argv) {
    bool list = false;
    while (argc > 1 && argv[1][0] == '-') {
        if (!strcmp(argv[1], "-l")) list = true;
        argv++; argc--;
    }
    if (argc < 2) { fprintf(stderr, "usage: scriptrun [-l] file.fmp12 < battery\n"); return 2; }
    signal(SIGSEGV, crashed);
    signal(SIGABRT, crashed);
    signal(SIGBUS, crashed);
    // Start-up as fmsased's FMScriptMgr::OnInit: engine controller, the
    // script engine's application info ("Server 26.0.3", type 7), Start(),
    // the design functions.
    static DBEngineController* ctl = new DBEngineController();
    ApplicationInfo::GetInstance()->SetApplicationInfo(unistring(u""), unistring(u"Server 26.0.3"), (ApplicationInfo::AppType)7);
    ctl->Start(true, 256u << 20, nullptr, false, false, false);
    InstallDesignFunction();
    DBAuthenticationInfo* auth = new DBAuthenticationInfo((DBAuthenticationInfo::AuthTypes)2);  // kSecurityByPass
    UniversalPath path;
    unistring p(from_utf8(argv[1]));
    path.SetCommandLineFormat(p);
    if (list) {
        std::vector<unistring> names;
        DBError e = SMSession::GetScriptNames(path, *auth, names, unistring());
        std::cout << "# GetScriptNames error " << e.code << "\n";
        for (auto& n : names) std::cout << to_utf8_escaped(n) << "\n";
        std::cout.flush();
        _exit(0);
    }
    memset(g_context.b, 0, sizeof g_context.b);
    if (getenv("STEP_LIMIT")) g_step_limit = atol(getenv("STEP_LIMIT"));
    std::string line;
    while (std::getline(std::cin, line)) {
        if (line.empty() || line[0] == '#') { std::cout << line << "\n"; continue; }
        std::vector<std::string> f = split_tabs(line);
        if (f[0] == "@watch") {
            g_watch.clear();
            for (size_t i = 1; i < f.size(); i++) g_watch.push_back(from_utf8(unescape(f[i])));
            std::cout << "# " << line << "\n";
            continue;
        }
        if (f[0] == "@call") {
            if (f.size() < 6) g_calls.clear();
            else g_calls.push_back({f[1], atoi(f[2].c_str()), f[3], (unsigned)atoi(f[4].c_str()), f[5] == "1"});
            std::cout << "# " << line << "\n";
            continue;
        }
        if (f[0] == "@scan") {
            scan_scripts(auth, argv[1], std::vector<std::string>(f.begin() + 1, f.end()));
            continue;
        }
        if (f[0] == "@trace off" || f[0] == "@trace on") { g_trace = f[0] == "@trace on"; std::cout << "# " << line << "\n"; continue; }
        if (f[0] == "@path" || f[0].compare(0, 6, "@path ") == 0) {
            unistring other(from_utf8(f[0].size() > 6 ? f[0].substr(6) : std::string(argv[1])));
            path = UniversalPath();
            path.SetCommandLineFormat(other);
            std::cout << "# " << line << "\n";
            continue;
        }
        if (f[0].compare(0, 9, "@context ") == 0) { g_context = parse_key(f[0].substr(9)); std::cout << "# " << line << "\n"; continue; }
        if (f[0].compare(0, 9, "@account ") == 0) {
            delete auth;
            auth = new DBAuthenticationInfo((DBAuthenticationInfo::AuthTypes)0);
            bool keep = false;
            auth->SetToUserNamePassword(unistring(from_utf8(f[0].substr(9))), unistring(from_utf8(f.size() > 1 ? f[1] : "")), keep);
            std::cout << "# @account " << f[0].substr(9) << "\n";
            continue;
        }
        unistring name(from_utf8(unescape(f[0])));
        unistring param(from_utf8(f.size() > 1 ? unescape(f[1]) : ""));
        g_post.clear();
        for (size_t i = 2; i < f.size(); i++) g_post.push_back(from_utf8(unescape(f[i])));
        g_trace_lines.clear();
        g_post_values.clear();
        g_step_count = 0;
        g_run_head = f[0] + "\t" + (f.size() > 1 ? f[1] : "");
        SMSession* s = new SMSession(true, *auth);
        unistring result;
        DBError e = s->RunScriptAndClose(path, *auth, name, param, &result);
        std::cout << f[0] << "\t" << (f.size() > 1 ? f[1] : "") << "\t" << e.code << "\t" << to_utf8_escaped(result);
        for (auto& v : g_post_values) std::cout << "\t" << v;
        std::cout << "\n";
        for (auto& t : g_trace_lines) std::cout << t << "\n";
        std::cout.flush();
        // sessions are leaked on purpose (their files are closed by
        // RunScriptAndClose)
    }
    std::cout.flush();
    _exit(0);
}
