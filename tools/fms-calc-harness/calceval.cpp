// Research harness, see README.md. Runs only inside the network-less amd64
// container. Opens a copy of an .fmp12 file with FileMaker Server 26's own
// engine (libDBEngine.so, libSupport.so, called through their exported entry
// points; no FileMaker code is copied) and reads lines from stdin. Input
// escapes: \n \r \t \\ (any other backslash is kept); \uXXXX is decoded
// too. Modes (first argument):
//   eval (default)  evaluate each expression with DBExpressionEnv::Evaluate:
//     expr <TAB> error <TAB> cachedType/nativeType/calcNativeType <TAB> result text
//     "@ <context> [record ID]" lines move to a table occurrence and record
//     (DBRecordCursor), "@rep <n>" sets the repetition being calculated.
//   -c  compile each formula the way DBCalcFormula::Set does (DBCalcParse::
//     DoParse, DBCalc::Optimize, DBCalc::WriteAsPolish, DBCalcScan::Tokenize):
//     expr <TAB> error <TAB> begin:end <TAB> context <TAB> formatted <TAB> polish (hex)
//   -d  (with any mode) also register the design functions (FMDR) by calling
//     libFMEngine's InstallDesignFunction()
//   -r  round trip stored formulas: "id <TAB> context <TAB> formatted <TAB> polish"
//     (hex) -> DBCalcScan::UnTokenize -> compile (again with kSupportSelf after
//     error 1225) -> compare:
//     id <TAB> error <TAB> sameFormatted <TAB> samePolish <TAB> options <TAB> text <TAB> formatted <TAB> polish
// Result text uses the input escapes plus \uXXXX for controls, private-use
// characters and lone surrogates. Types are Draco::DBDataType values (1 text, 2 number, 3 date,
// 4 time, 5 timestamp).
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
class Locale;
class DBTableProxy { public: long long GetRowCount() const; };
class DBFileUser {
public:
    static Locale const& GetLocale(DBFileUser const*);
    DBTableProxy& GetTableProxy(HBSerialKey<5> const&) const;
};
class DBTableAliasCatalog;
class FMCertVerifyCallBackInfo;
class DBCursor;
// A cursor on one record: DBRecordCursor(file, table occurrence, record key)
class DBRecordCursor { public: alignas(16) char b[0x800]; DBRecordCursor(DBFileUser const&, HBSerialKey<5> const&, HBSerialKey<9> const&); };
struct RetErr { int code; RetErr() : code(0) {} RetErr(const RetErr& o) : code(o.code) {} ~RetErr() {} };
class DBAuthenticationInfo {
public:
    enum AuthTypes : int {};
    alignas(16) char b[0x1000];
    DBAuthenticationInfo(AuthTypes);
    ~DBAuthenticationInfo();
};
class HBEncryptionInfo;
class DBEngineController { public: alignas(16) char b[0x10000]; DBEngineController(); void Start(bool, unsigned int, HBEncryptionInfo*, bool, bool, bool); };
class ApplicationInfo { public: enum AppType : int {}; static ApplicationInfo* GetInstance(); void SetApplicationInfo(unistring const&, unistring const&, AppType); };
class DBSession {
public:
    alignas(16) char b[0x10000];
    DBSession(DBAuthenticationInfo const&);
    RetErr OpenFileUser(std::shared_ptr<DBFileUser>&, std::vector<UniversalPath> const&, DBFileUser const*,
                        DBAuthenticationInfo const&, unsigned int, FMCertVerifyCallBackInfo*);
};
enum DBDataType : unsigned int {};
class Text { public: alignas(16) char b[0x400]; Text(); Text(char16_t const*, unsigned int); ~Text(); void DumpRunsToXML(unistring&) const; };
class TextBuffer { public: void Get(unistring&, unsigned int, unsigned int) const; };
class DBData { public: alignas(16) char b[0x400]; DBData(); ~DBData(); Text const& GetAsText() const; void ConvertData(DBDataType); };
// Error and DBError are {short code; ushort extended-error index}, returned in
// memory (non-trivial copy and destructor).
class DBError { public: short code; unsigned short index; DBError() : code(0), index(0) {} DBError(const DBError& o) : code(o.code), index(o.index) {} ~DBError() {} };
class DBScanError { public: alignas(16) char b[0x40]; DBScanError(int); ~DBScanError(); bool Retrieve(DBError const&); };
// SwapOStream holds a std::ostream&; SwapIStream is a RefCountedObj (vptr,
// flags, count) followed by a std::istream&.
class SwapOStream { public: std::ostream* s; };
class SwapIStream { public: void* vptr; long rc; std::istream* s; };
class DBCalc;
class DBBindingEnv { public: alignas(16) char b[0x100]; DBBindingEnv(); ~DBBindingEnv(); };
class DBCalcScan {
public:
    alignas(16) char b[0x400];
    DBCalcScan(Locale const&, DBTableAliasCatalog*, Text const*);
    ~DBCalcScan();
    DBError Tokenize(SwapOStream&, Text const*, HBSerialKey<5> const&);
    DBError UnTokenize(Text&, SwapIStream&) const;
};
class DBCalcParse {
public:
    alignas(16) char b[0x400];  // mScanner (DBCalcScan) at 0
    DBCalcParse(Locale const&, DBTableAliasCatalog*);
    ~DBCalcParse();
    DBError DoParse(DBCalc*&, Text const*, HBSerialKey<5> const&, DBBindingEnv*);
};
class DBExpressionEnv {
public:
    alignas(16) char b[0x100];
    void Setup(DBFileUser const*, HBSerialKey<5> const&, DBCursor const*, DBCursor const*);
    int Evaluate(Text const&, DBData&) const;
};
}  // namespace Draco
extern "C" char _ZTVN5Draco11SwapIStreamE[];  // vtable for Draco::SwapIStream

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
        if (cp >= 0xD800 && cp < 0xDC00 && i + 1 < s.size() && s[i + 1] >= 0xDC00 && s[i + 1] < 0xE000) { cp = 0x10000 + ((cp - 0xD800) << 10) + (s[i + 1] - 0xDC00); i++; }
        if (cp == '\\') { out += "\\\\"; continue; }
        if (cp == '\r') { out += "\\r"; continue; }
        if (cp == '\n') { out += "\\n"; continue; }
        if (cp == '\t') { out += "\\t"; continue; }
        if (cp < 0x20 || (cp >= 0xD800 && cp < 0xF900)) { char buf[16]; snprintf(buf, sizeof buf, "\\u%04X", cp); out += buf; continue; }
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
static std::string hex(const std::string& b) {
    static const char* d = "0123456789abcdef";
    std::string out;
    for (unsigned char c : b) { out += d[c >> 4]; out += d[c & 15]; }
    return out;
}
static std::string unhex(const std::string& h) {
    std::string out;
    for (size_t i = 0; i + 1 < h.size(); i += 2) { unsigned v; sscanf(h.c_str() + i, "%2x", &v); out += (char)v; }
    return out;
}
// Table occurrence keys are given and printed as the formula's stored context
// (child 4, e.g. 03d00002 or 0100); the HBSerialKey<5> is that string with
// its length byte in front (04 03 d0 00 02). An empty string is the zero key.
static HBSerialKey<5> parse_key(const std::string& h) {
    HBSerialKey<5> k; memset(k.b, 0, sizeof k.b);
    std::string b = unhex(h);
    if (b.size() > 5) b.resize(5);
    k.b[0] = (unsigned char)b.size();
    memcpy(k.b + 1, b.data(), b.size());
    return k;
}
static std::string key_hex(const HBSerialKey<5>& k) { return hex(std::string((const char*)k.b + 1, k.b[0] < 6 ? k.b[0] : 5)); }
static std::u16string text_of(const Text& t) {
    unistring u;
    ((const TextBuffer*)((const char*)&t + 0x130))->Get(u, 0, 0x7fffffff);
    return u;
}

// Virtual members of DBCalc (DWARF DW_AT_vtable_elem_location): 1 deleting
// destructor, 15 Optimize(unsigned) -> DBCalc*, 16 WriteAsPolish(SwapOStream&)
// const -> DBError.
typedef DBCalc* (*OptimizeFn)(DBCalc*, unsigned);
typedef DBError (*WritePolishFn)(const DBCalc*, SwapOStream*);
typedef void (*DeleteFn)(DBCalc*);
static void** vtbl(DBCalc* c) { return *(void***)c; }

struct Compiled { int err = 0; unsigned begin = 0, end = 0; std::string formatted, polish; };
// The steps of DBCalcFormula::Set without the HBPosition writes: a parser on
// the file's locale and table occurrence catalog, DoParse in the context
// table occurrence, Optimize(1), WriteAsPolish into memory, then Tokenize of
// the same text with the same scanner. options = DBCalcFormula::CalcOptions
// (16 kSupportSelf, 1/2/4 auto-quoting).
static Compiled compile(DBFileUser* fu, const std::u16string& src, const HBSerialKey<5>& ctx, unsigned options) {
    Compiled r;
    DBTableAliasCatalog* cat = (DBTableAliasCatalog*)((char*)fu + 0xa00);  // DBFileUser::mTableAliasCatalog
    DBCalcParse* parser = new DBCalcParse(DBFileUser::GetLocale(fu), cat);
    *(unsigned*)(parser->b + 0x50) = options;  // DBCalcScan::mOptions
    Text t(src.data(), (unsigned)src.size());
    DBCalc* root = nullptr;
    DBBindingEnv* benv = new DBBindingEnv();
    DBError e = parser->DoParse(root, &t, ctx, benv);
    r.err = e.code;
    if (e.code) {
        DBScanError se(0);
        if (se.Retrieve(e)) { r.begin = *(unsigned*)(se.b + 0x18); r.end = *(unsigned*)(se.b + 0x1c); }
    } else if (root) {
        root = ((OptimizeFn)vtbl(root)[15])(root, 1);
        std::ostringstream pol;
        SwapOStream ps{&pol};
        DBError we = ((WritePolishFn)vtbl(root)[16])(root, &ps);
        if (we.code) r.err = we.code;
        std::ostringstream fmt;
        SwapOStream fs{&fmt};
        DBError te = ((DBCalcScan*)parser)->Tokenize(fs, &t, ctx);
        if (te.code && !r.err) r.err = te.code;
        r.polish = pol.str();
        r.formatted = fmt.str();
    }
    if (root) ((DeleteFn)vtbl(root)[1])(root);
    delete benv;
    delete parser;
    return r;
}
// DBCalcScan::UnTokenize: the engine's display text of a formatted stream.
static int untokenize(DBFileUser* fu, const std::string& formatted, std::u16string& out) {
    DBTableAliasCatalog* cat = (DBTableAliasCatalog*)((char*)fu + 0xa00);
    DBCalcScan* scan = new DBCalcScan(DBFileUser::GetLocale(fu), cat, nullptr);
    std::istringstream is(formatted);
    SwapIStream in{_ZTVN5Draco11SwapIStreamE + 16, 1L << 32, &is};
    Text t;
    DBError e = scan->UnTokenize(t, in);
    out = text_of(t);
    delete scan;
    return e.code;
}

static std::vector<std::string> split_tabs(const std::string& s) {
    std::vector<std::string> f;
    size_t a = 0, b;
    while ((b = s.find('\t', a)) != std::string::npos) { f.push_back(s.substr(a, b - a)); a = b + 1; }
    f.push_back(s.substr(a));
    return f;
}

int main(int argc, char** argv) {
    char mode = 'e';
    bool design = false;
    while (argc > 1 && argv[1][0] == '-') {
        for (const char* o = argv[1] + 1; *o; o++) { if (*o == 'd') design = true; else mode = *o; }
        argv++; argc--;
    }
    if (argc < 2) { fprintf(stderr, "usage: calceval [-c|-r] [-d] file.fmp12 [context table occurrence, stored form, hex]\n"); return 2; }
    signal(SIGSEGV, crashed);
    signal(SIGABRT, crashed);
    signal(SIGBUS, crashed);
    // Same start-up sequence as FMDeveloperTool: application info, engine
    // controller Start(), a security-bypass session, OpenFileUser.
    static DBEngineController* ctl = new DBEngineController();
    ApplicationInfo::GetInstance()->SetApplicationInfo(unistring(u""), unistring(u"FMDeveloperTool 26.0.3.309 (08-26-2026)"), (ApplicationInfo::AppType)0x63);
    ctl->Start(true, 256u << 20, nullptr, false, false, false);
    DBAuthenticationInfo* auth = new DBAuthenticationInfo((DBAuthenticationInfo::AuthTypes)2);  // kSecurityByPass, as FMDeveloperTool
    DBSession* sess = new DBSession(*auth);
    std::vector<UniversalPath> paths(1);
    unistring p(from_utf8(argv[1]));
    paths[0].SetCommandLineFormat(p);
    std::shared_ptr<DBFileUser> fu;
    RetErr e = sess->OpenFileUser(fu, paths, nullptr, *auth, 0, nullptr);
    fprintf(stderr, "open: err=%d fileUser=%p\n", e.code, (void*)fu.get());
    if (!fu) return 1;
    if (design) {
        // -d: register the design functions (FMDR) the way FileMaker's
        // libFMEngine does at start-up, through its exported
        // Draco::InstallDesignFunction() (DBCalcScan::RegisterExternalFunction).
        void* h = dlopen("libFMEngine.so", RTLD_NOW | RTLD_GLOBAL);
        void* f = h ? dlsym(h, "_ZN5Draco21InstallDesignFunctionEv") : nullptr;
        if (f) ((void (*)())f)();
        fprintf(stderr, "design functions: %s\n", f ? "installed" : dlerror());
    }
    HBSerialKey<5> tkey = parse_key(argc > 2 ? argv[2] : "");
    std::string line;
    if (mode == 'c') {
        // prefixes: "@HEX " context table occurrence key for this line; "%N " CalcOptions
        while (std::getline(std::cin, line)) {
            if (line.empty() || line[0] == '#') { std::cout << line << "\n"; continue; }
            HBSerialKey<5> ctx = tkey; unsigned options = 0; std::string body = line;
            for (;;) {
                size_t sp = body.find(' ');
                if (body.size() > 1 && body[0] == '@' && sp != std::string::npos) { ctx = parse_key(body.substr(1, sp - 1)); body = body.substr(sp + 1); continue; }
                if (body.size() > 1 && body[0] == '%' && sp != std::string::npos) { options = atoi(body.c_str() + 1); body = body.substr(sp + 1); continue; }
                break;
            }
            Compiled c = compile(fu.get(), from_utf8(unescape(body)), ctx, options);
            std::cout << line << "\t" << c.err << "\t" << c.begin << ":" << c.end << "\t" << key_hex(ctx) << "\t" << hex(c.formatted) << "\t" << hex(c.polish) << "\n";
            std::cout.flush();
        }
        std::cout.flush();
        _exit(0);
    }
    if (mode == 'r') {
        while (std::getline(std::cin, line)) {
            if (line.empty() || line[0] == '#') continue;
            std::vector<std::string> f = split_tabs(line);
            if (f.size() < 4) continue;
            std::u16string src;
            int ue = untokenize(fu.get(), unhex(f[2]), src);
            Compiled c;
            unsigned options = 0;
            if (!ue) c = compile(fu.get(), src, parse_key(f[1]), options);
            if (c.err == 1225) c = compile(fu.get(), src, parse_key(f[1]), options = 16);  // Self: kSupportSelf
            std::string fmt = unhex(f[2]), pol = unhex(f[3]);
            std::cout << f[0] << "\t" << (ue ? ue : c.err) << "\t" << (c.formatted == fmt) << "\t" << (c.polish == pol) << "\t" << options << "\t"
                      << to_utf8_escaped(src) << "\t" << hex(c.formatted) << "\t" << hex(c.polish) << "\n";
            std::cout.flush();
        }
        std::cout.flush();
        _exit(0);
    }
    DBExpressionEnv* env = (DBExpressionEnv*)calloc(1, sizeof(DBExpressionEnv));
    env->Setup(fu.get(), tkey, nullptr, nullptr);
    DBCursor* cursor = nullptr;
    while (std::getline(std::cin, line)) {
        if (line.empty() || line[0] == '#') { std::cout << line << "\n"; continue; }
        // "@ <context> [id]": evaluate in table occurrence <context> (stored
        // form) on the record with record ID id (DBRecordCursor), or with no
        // record when id is absent. "@rep <n>": DBExpressionEnv::mRepetition.
        if (line.compare(0, 5, "@rep ") == 0) {
            *(short*)(env->b + 0x20) = (short)atoi(line.c_str() + 5);
            std::cout << "# " << line << "\n";
            continue;
        }
        if (line.compare(0, 2, "@ ") == 0) {
            std::istringstream ls(line.substr(2));
            std::string ctx; unsigned long long id = 0;
            ls >> ctx >> id;
            HBSerialKey<5> k = parse_key(ctx);
            short rep = *(short*)(env->b + 0x20);
            std::string info = " records=" + std::to_string(fu->GetTableProxy(k).GetRowCount());
            cursor = nullptr;  // previous cursors are leaked on purpose
            if (id) {
                // record key: one key segment holding the ID, big-endian, minimal length
                HBSerialKey<9> rk; memset(rk.b, 0, sizeof rk.b);
                unsigned char be[8]; int n = 0;
                for (unsigned long long v = id; v; v >>= 8) be[n++] = (unsigned char)(v & 0xff);
                rk.b[0] = (unsigned char)(n + 1); rk.b[1] = (unsigned char)n;
                for (int i = 0; i < n; i++) rk.b[2 + i] = be[n - 1 - i];
                cursor = (DBCursor*)new DBRecordCursor(*fu, k, rk);
            }
            env->Setup(fu.get(), k, cursor, nullptr);
            if (rep) *(short*)(env->b + 0x20) = rep;
            std::cout << "# " << line << info << "\n";
            continue;
        }
        // prefixes: "!N " convert result to DBDataType N (1 text 2 number 3 date 4 time 5 timestamp);
        //           "~ " also dump style runs of the result text
        int conv = 0; bool runs = false; std::string body = line;
        for (;;) {
            if (body.size() > 2 && body[0] == '!' && body[1] >= '0' && body[1] <= '9' && body[2] == ' ') { conv = body[1] - '0'; body = body.substr(3); continue; }
            if (body.size() > 1 && body[0] == '~' && body[1] == ' ') { runs = true; body = body.substr(2); continue; }
            break;
        }
        std::u16string ex = from_utf8(unescape(body));
        Text t(ex.data(), (unsigned)ex.size());
        DBData d;
        int err = env->Evaluate(t, d);
        if (conv) d.ConvertData((DBDataType)conv);
        const Text& rt = d.GetAsText();
        unistring u = text_of(rt);
        int* ty = (int*)d.b;
        std::string extra;
        if (runs) { unistring x; rt.DumpRunsToXML(x); extra = "\t" + to_utf8_escaped(x); }
        std::cout << line << "\t" << err << "\t" << ty[0] << "/" << ty[1] << "/" << ty[2] << "\t" << to_utf8_escaped(u) << extra << "\n";
        std::cout.flush();
    }
    // Leave without static destructors: a libSupport timer thread can crash
    // while they run.
    std::cout.flush();
    _exit(0);
}
