"""Recover the RCTransaction opcode registry from an llvm-objdump disassembly.

Usage: python3 -I rc_registry.py <thin-macho> <disassembly.txt> <demangled-symbols.txt>

Finds calls to Draco::RCNetworkStack::RegisterTransactionClass and
RegisterTransactionNameOnly, recovers the opcode immediate (esi), the name
string (rcx/rsi lea), and the creation function (rdx, via GOT or lea).
The binary is only read as data; nothing is executed.
"""
import re
import struct
import sys

macho, dis, syms = sys.argv[1:4]
data = open(macho, 'rb').read()

addr2sym = {}
for line in open(syms, errors='replace'):
    parts = line.rstrip('\n').split(' ', 2)
    if len(parts) == 3 and parts[1] in ('T', 't'):
        addr2sym.setdefault(int(parts[0], 16), parts[2])


def cstr(addr):
    end = data.index(b'\0', addr)
    return data[addr:end].decode('utf-8', 'replace')


gotbind = {}
if len(sys.argv) > 4:
    # optional: '<addr> <symbol>' lines from llvm-objdump --macho --dyld-info
    for line in open(sys.argv[4]):
        a, s_ = line.split()
        gotbind[int(a, 16)] = s_


def got(addr):
    raw = struct.unpack_from('<Q', data, addr)[0]
    if raw >> 63:
        return gotbind.get(addr, 'extern')
    return raw & ((1 << 36) - 1)


reg_re = re.compile(r'callq\s+0x[0-9a-f]+ <__ZN5Draco14RCNetworkStack(24RegisterTransactionClass|27RegisterTransactionNameOnly)')
ins_re = re.compile(r'^\s*([0-9a-f]+):\s+(\S+)\s+(.*)$')
window = []
for line in open(dis, errors='replace'):
    m = ins_re.match(line)
    if not m:
        continue
    rest = m.group(3)
    ops, _, cmt = rest.partition('##')
    window.append((int(m.group(1), 16), m.group(2), ops.strip(), cmt.strip()))
    if len(window) > 14:
        window.pop(0)
    if not reg_re.search(line):
        continue
    kind = 'class' if 'Class' in line else 'name-only'
    op = name = fn = None
    for _, mn, args, cmt in reversed(window[:-1]):
        if op is None and mn in ('movl', 'movb') and (args.endswith('%esi') or args.endswith('%sil')):
            mm = re.match(r'\$(-?0x[0-9a-f]+|-?\d+), %(esi|sil)', args)
            if mm:
                op = int(mm.group(1), 0)
        if mn == 'leaq' and name is None and (args.split(',')[-1].strip() == ('%rcx' if kind == 'class' else '%rdx')):
            mm = re.match(r'0x([0-9a-f]+)', cmt)
            if mm:
                name = cstr(int(mm.group(1), 16))
        if kind == 'class' and fn is None and args.split(',')[-1].strip() == '%rdx':
            mm = re.match(r'0x([0-9a-f]+)', cmt)
            if mm:
                a = int(mm.group(1), 16)
                tgt = got(a) if mn == 'movq' else a
                fn = tgt if isinstance(tgt, str) else addr2sym.get(tgt, hex(tgt))
    if op is not None:
        op &= 0xff
        if op > 127:
            op -= 256
    print(f'{window[-1][0]:x}\t{op}\t{kind}\t{name}\t{fn}')
