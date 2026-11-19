"""Print string references, inline ASCII immediates, and calls inside an address range
of an llvm-objdump disassembly. Usage: python3 -I fn_strings.py <macho> <dis> <start> <end> [--calls]"""
import re
import struct
import sys

macho, dis, start, end = sys.argv[1], sys.argv[2], int(sys.argv[3], 16), int(sys.argv[4], 16)
show_calls = '--calls' in sys.argv
data = open(macho, 'rb').read()
stubs = {}
for arg in sys.argv[5:]:
    if arg.startswith('--stubs='):
        for line in open(arg[8:]):
            a, s_ = line.split()
            stubs[int(a, 16)] = s_
ins_re = re.compile(r'^\s*([0-9a-f]+):\s+(\S+)\s+(.*)$')


def printable(b):
    return all(32 <= c < 127 for c in b)


def cstr(addr):
    try:
        end_ = data.index(b'\0', addr, addr + 400)
    except ValueError:
        return None
    raw = data[addr:end_]
    if len(raw) >= 1 and printable(raw):
        return raw.decode()
    return None


for line in open(dis, errors='replace'):
    m = ins_re.match(line)
    if not m:
        continue
    a = int(m.group(1), 16)
    if a < start:
        continue
    if a >= end:
        break
    mn, rest = m.group(2), m.group(3)
    ops, _, cmt = rest.partition('##')
    if mn == 'leaq':
        mm = re.match(r'\s*0x([0-9a-f]+)', cmt)
        if mm:
            s = cstr(int(mm.group(1), 16))
            if s:
                print(f'{a:x} lea  {ops.split(",")[-1].strip():5} "{s}"')
    elif mn in ('movabsq', 'movq', 'movl') and ops.startswith('$0x'):
        imm = int(ops.split(',')[0][1:], 16)
        n = 8 if mn != 'movl' else 4
        b = imm.to_bytes(8, 'little')[:n].rstrip(b'\0')
        if len(b) >= 3 and printable(b):
            print(f'{a:x} imm  {ops.split(",")[-1].strip():5} "{b.decode()}"   ({ops.split(",")[-1].strip()})')
    elif show_calls and mn == 'callq' and '<' in rest:
        tgt = int(rest.split()[0], 16) if rest.startswith('0x') else None
        name = stubs.get(tgt) or rest[rest.index('<') + 1:].rstrip('>')
        print(f'{a:x} call {name[:150]}')
