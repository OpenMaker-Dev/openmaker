// Research harness, see README.md. Runs only inside the network-less amd64
// container of tools/fms-calc-harness. Loads FileMaker Server 26's
// libDBEngine.so and the ANTLR 4.13.1 runtime it ships, constructs the SQL
// lexer and parsers FileMaker generated from its grammar (FQLSQLLexer,
// FQLSQLParser, FQLSQLSemanticAnalyzer) and calls their exported accessors.
// No FileMaker code is copied: only exported entry points are declared
// (names and ABI from the dynamic symbol table).
//   fqlgrammar atn    print rule names, vocabulary and the serialized ATN of
//                     all three recognizers (input of atn_grammar.py)
//   fqlgrammar tree   read SQL statements from stdin (one per line, escapes
//                     \n \r \t \\), lex them the way FQL::FQLParserDriver::
//                     CreateTokenStream does and parse them with
//                     FQLSQLSemanticAnalyzer::statement(), the rule
//                     FQLParserDriver::ParseQuery runs; print the tokens, the
//                     number of syntax errors and the parse tree
#include <cstdio>
#include <cstdint>
#include <cstring>
#include <cstdlib>
#include <string>
#include <string_view>
#include <vector>
#include <iostream>
#include <unistd.h>
#include <dlfcn.h>

struct ATNView { const int32_t* data; size_t size; };
typedef const std::vector<std::string>& (*NamesFn)(const void*);
typedef const void* (*VocabFn)(const void*);
typedef ATNView (*ATNFn)(const void*);

// FileMaker's generated recognizers (libDBEngine.so)
void lexer_ctor(void*, void* charStream) asm("_ZN11FQLSQLLexerC1EPN6antlr410CharStreamE");
const std::vector<std::string>& lexer_rules(const void*) asm("_ZNK11FQLSQLLexer12getRuleNamesB5cxx11Ev");
const std::vector<std::string>& lexer_modes(const void*) asm("_ZNK11FQLSQLLexer12getModeNamesB5cxx11Ev");
const std::vector<std::string>& lexer_channels(const void*) asm("_ZNK11FQLSQLLexer15getChannelNamesB5cxx11Ev");
const void* lexer_vocab(const void*) asm("_ZNK11FQLSQLLexer13getVocabularyEv");
ATNView lexer_atn(const void*) asm("_ZNK11FQLSQLLexer16getSerializedATNEv");
void parser_ctor(void*, void* tokenStream) asm("_ZN12FQLSQLParserC1EPN6antlr411TokenStreamE");
const std::vector<std::string>& parser_rules(const void*) asm("_ZNK12FQLSQLParser12getRuleNamesB5cxx11Ev");
const void* parser_vocab(const void*) asm("_ZNK12FQLSQLParser13getVocabularyEv");
ATNView parser_atn(const void*) asm("_ZNK12FQLSQLParser16getSerializedATNEv");
void analyzer_ctor(void*, void* tokenStream) asm("_ZN22FQLSQLSemanticAnalyzerC1EPN6antlr411TokenStreamE");
const std::vector<std::string>& analyzer_rules(const void*) asm("_ZNK22FQLSQLSemanticAnalyzer12getRuleNamesB5cxx11Ev");
const void* analyzer_vocab(const void*) asm("_ZNK22FQLSQLSemanticAnalyzer13getVocabularyEv");
ATNView analyzer_atn(const void*) asm("_ZNK22FQLSQLSemanticAnalyzer16getSerializedATNEv");
void* analyzer_statement(void*) asm("_ZN22FQLSQLSemanticAnalyzer9statementEv");

// ANTLR 4.13.1 runtime (libantlr4-runtime.so)
std::string_view vocab_literal(const void*, size_t) asm("_ZNK6antlr43dfa10Vocabulary14getLiteralNameEm");
std::string_view vocab_symbolic(const void*, size_t) asm("_ZNK6antlr43dfa10Vocabulary15getSymbolicNameEm");
void input_ctor(void*, std::string_view) asm("_ZN6antlr416ANTLRInputStreamC1ESt17basic_string_viewIcSt11char_traitsIcEE");
void tokens_ctor(void*, void* tokenSource) asm("_ZN6antlr417CommonTokenStreamC1EPNS_11TokenSourceE");
void tokens_fill(void*) asm("_ZN6antlr419BufferedTokenStream4fillEv");
std::vector<void*> tokens_get(void*) asm("_ZN6antlr419BufferedTokenStream9getTokensEv");
std::string token_string(const void* token, void* recognizer) asm("_ZNK6antlr411CommonToken8toStringB5cxx11EPNS_10RecognizerE");
size_t parser_errors(void*) asm("_ZN6antlr46Parser23getNumberOfSyntaxErrorsEv");
std::string tree_string(void* tree, void* parser, bool pretty) asm("_ZN6antlr44tree5Trees12toStringTreeB5cxx11EPNS0_9ParseTreeEPNS_6ParserEb");

// The names FQL::FQLParserDriver::LookupFunctionName compares (case
// insensitively) an IDENTIFIER '(' ... ')' call against: 20 entries of a
// Draco::unistring (std::u16string layout) and the int it returns, filled by
// a static initializer when libDBEngine.so loads.
extern char FunctionTable[] asm("FunctionTable");
// FQL::NonLocalizedStrings(int): the FQLnnnn message texts (index = nnnn);
// FQL::ErrorToString(int): the text ExecuteSQLe shows for an engine error code.
std::u16string fql_message(int) asm("_ZN3FQL19NonLocalizedStringsEi");
std::u16string error_text(int) asm("_ZN3FQL13ErrorToStringEi");
// libSupport: the collation object of a Locale::Type (2 = Unicode raw) and
// its text key builder International::MakeKey(buffer, length in/out, text,
// length, bool, int).
const void* fixed_international(int type) asm("_ZN5Draco6Locale19FixedInternationalsENS0_4TypeE");
void make_key(const void* intl, unsigned char* buf, unsigned char& len, const char16_t* text, unsigned n, bool b, int i)
    asm("_ZNK5Draco13International7MakeKeyEPhRhPKDsjbi");

// Object sizes and the Lexer -> TokenSource base offset as
// FQL::FQLParserDriver::CreateTokenStream allocates and converts them
// (operator new(0x50) ANTLRInputStream, operator new(0x110) FQLSQLLexer,
// TokenSource at +0x78, operator new(0x40) CommonTokenStream).
static void* blob(size_t n) { void* p = aligned_alloc(16, n); memset(p, 0, n); return p; }

static std::string esc(std::string_view s) {
    std::string o;
    for (unsigned char c : s) {
        if (c == '\\') o += "\\\\"; else if (c == '\n') o += "\\n"; else if (c == '\r') o += "\\r"; else if (c == '\t') o += "\\t";
        else if (c < 0x20) { char b[8]; snprintf(b, sizeof b, "\\x%02x", c); o += b; }
        else o += (char)c;
    }
    return o;
}
static std::string unescape(const std::string& s) {
    std::string out;
    for (size_t i = 0; i < s.size(); i++) {
        if (s[i] == '\\' && i + 1 < s.size()) {
            char n = s[++i];
            if (n == 'n') out += '\n'; else if (n == 'r') out += '\r'; else if (n == 't') out += '\t'; else if (n == '\\') out += '\\';
            else { out += '\\'; out += n; }
        } else out += s[i];
    }
    return out;
}

static void dump(const char* name, const std::vector<std::string>& rules, const void* vocab, ATNView atn) {
    printf("recognizer\t%s\n", name);
    for (size_t i = 0; i < rules.size(); i++) printf("rule\t%zu\t%s\n", i, rules[i].c_str());
    // maxTokenType is the third int of the serialized ATN (version, grammar type, max token type)
    int max = atn.size > 2 ? atn.data[2] : 0;
    for (int t = 0; t <= max; t++) {
        std::string_view l = vocab_literal(vocab, t), s = vocab_symbolic(vocab, t);
        printf("token\t%d\t%s\t%s\n", t, esc(l).c_str(), esc(s).c_str());
    }
    printf("atn\t%zu", atn.size);
    for (size_t i = 0; i < atn.size; i++) printf(" %d", atn.data[i]);
    printf("\n");
}

static std::string utf8(const std::u16string& s) {
    std::string o;
    for (size_t i = 0; i < s.size(); i++) {
        unsigned cp = s[i];
        if (cp >= 0xD800 && cp < 0xDC00 && i + 1 < s.size()) { cp = 0x10000 + ((cp - 0xD800) << 10) + (s[i + 1] - 0xDC00); i++; }
        if (cp == 0x7f) { o += "^"; continue; }  // placeholder marker before a digit: ^0, ^1
        if (cp < 0x80) o += (char)cp;
        else if (cp < 0x800) { o += (char)(0xC0 | (cp >> 6)); o += (char)(0x80 | (cp & 0x3f)); }
        else if (cp < 0x10000) { o += (char)(0xE0 | (cp >> 12)); o += (char)(0x80 | ((cp >> 6) & 0x3f)); o += (char)(0x80 | (cp & 0x3f)); }
        else { o += (char)(0xF0 | (cp >> 18)); o += (char)(0x80 | ((cp >> 12) & 0x3f)); o += (char)(0x80 | ((cp >> 6) & 0x3f)); o += (char)(0x80 | (cp & 0x3f)); }
    }
    return esc(o);
}

int main(int argc, char** argv) {
    std::string mode = argc > 1 ? argv[1] : "atn";
    if (mode == "locale") {
        // FQL::FQLLocale is a file-local Draco::Locale at link address
        // 0x36d3e00 of libDBEngine.so (load base from dladdr of an exported
        // FQL symbol). Prints its first 32 bytes and the International of
        // each Locale::Type 0..80 whose address equals the pointer at +8.
        Dl_info info;
        void* f = dlsym(RTLD_DEFAULT, "_ZN3FQL14ExecuteSQLCalcERKN5Draco10DBFileUserERKNS0_9unistringES6_S6_RN9__gnu_cxx17__normal_iteratorIPKPNS0_6DBCalcESt6vectorISA_SaISA_EEEERKSG_RNS0_6DBDataEtb");
        if (!f || !dladdr(f, &info)) { printf("no symbol\n"); _exit(1); }
        const unsigned char* loc = (const unsigned char*)info.dli_fbase + 0x36d3e00;
        printf("FQLLocale");
        for (int i = 0; i < 32; i++) printf(" %02x", loc[i]);
        printf("\n");
        const void* intl = *(const void* const*)(loc + 8);
        for (int t = 0; t <= 80; t++)
            if (fixed_international(t) == intl) printf("International of Locale::Type %d\n", t);
        fflush(stdout);
        _exit(0);
    }
    if (mode == "keys") {
        // fqlgrammar keys <Locale::Type> [bool] [int] < strings (escapes \n \r \t \\ \uXXXX)
        int type = argc > 2 ? atoi(argv[2]) : 2;
        bool b = argc > 3 && atoi(argv[3]);
        int iv = argc > 4 ? atoi(argv[4]) : 0;
        const void* intl = fixed_international(type);
        std::string line;
        while (std::getline(std::cin, line)) {
            std::string s = unescape(line);
            std::u16string u;
            for (size_t i = 0; i < s.size(); i++) {
                if (s[i] == '\\' && i + 5 < s.size() + 1 && s[i + 1] == 'u') { u.push_back((char16_t)strtol(s.substr(i + 2, 4).c_str(), nullptr, 16)); i += 5; continue; }
                unsigned char c = s[i];
                if (c < 0x80) { u.push_back(c); continue; }
                unsigned cp; int n;
                if ((c >> 5) == 6) { cp = c & 0x1f; n = 2; } else if ((c >> 4) == 14) { cp = c & 0x0f; n = 3; } else { cp = c & 7; n = 4; }
                for (int k = 1; k < n; k++) cp = (cp << 6) | (s[i + k] & 0x3f);
                i += n - 1;
                if (cp >= 0x10000) { cp -= 0x10000; u.push_back(0xD800 + (cp >> 10)); u.push_back(0xDC00 + (cp & 0x3ff)); } else u.push_back(cp);
            }
            unsigned char buf[600]; memset(buf, 0, sizeof buf);
            unsigned char len = 255;
            make_key(intl, buf, len, u.data(), (unsigned)u.size(), b, iv);
            printf("%s\t%u\t", line.c_str(), len);
            for (unsigned i = 0; i < len; i++) printf("%02x", buf[i]);
            printf("\n");
        }
        fflush(stdout);
        _exit(0);
    }
    if (mode == "errors") {
        for (int i = 0; i <= 56; i++) printf("fql\t%d\t%s\n", i, utf8(fql_message(i)).c_str());
        for (int i = -1; i <= 40000; i++) {
            std::u16string t = error_text(i);
            if (!t.empty()) printf("error\t%d\t%s\n", i, utf8(t).c_str());
        }
        fflush(stdout);
        _exit(0);
    }
    if (mode == "atn") {
        void* in = blob(0x50);
        input_ctor(in, std::string_view(""));
        void* lex = blob(0x110);
        lexer_ctor(lex, in);
        void* ts = blob(0x40);
        tokens_ctor(ts, (char*)lex + 0x78);
        void* par = blob(0x1000);
        parser_ctor(par, ts);
        void* ana = blob(0x1000);
        analyzer_ctor(ana, ts);
        dump("FQLSQLLexer", lexer_rules(lex), lexer_vocab(lex), lexer_atn(lex));
        for (auto& m : lexer_modes(lex)) printf("mode\t%s\n", m.c_str());
        for (auto& c : lexer_channels(lex)) printf("channel\t%s\n", c.c_str());
        dump("FQLSQLParser", parser_rules(par), parser_vocab(par), parser_atn(par));
        dump("FQLSQLSemanticAnalyzer", analyzer_rules(ana), analyzer_vocab(ana), analyzer_atn(ana));
        for (int i = 0; i < 20; i++) {
            const char* e = FunctionTable + i * 0x28;
            const std::u16string& name = *(const std::u16string*)e;
            std::string n;
            for (char16_t c : name) n += c < 0x80 ? (char)c : '?';
            printf("function\t%d\t%s\t%d\n", i, n.c_str(), *(const int*)(e + 0x20));
        }
        fflush(stdout);
        _exit(0);
    }
    std::string line;
    while (std::getline(std::cin, line)) {
        if (line.empty() || line[0] == '#') { std::cout << line << "\n"; continue; }
        std::string sql = unescape(line);
        void* in = blob(0x50);
        input_ctor(in, std::string_view(sql));
        void* lex = blob(0x110);
        lexer_ctor(lex, in);
        void* ts = blob(0x40);
        tokens_ctor(ts, (char*)lex + 0x78);
        void* ana = blob(0x1000);
        analyzer_ctor(ana, ts);
        tokens_fill(ts);
        std::vector<void*> toks = tokens_get(ts);
        std::cout << "sql\t" << esc(sql) << "\n";
        for (void* t : toks) std::cout << "  token\t" << token_string(t, ana) << "\n";
        std::cout.flush();
        void* tree = analyzer_statement(ana);
        std::cout << "  errors\t" << parser_errors(ana) << "\n";
        std::cout << "  tree\t" << tree_string(tree, ana, false) << "\n";
        std::cout.flush();
    }
    _exit(0);
}
