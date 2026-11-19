"""Map addresses to the containing exported function. Usage: python3 -I addr2func.py <funcs.sorted> <hex>..."""
import bisect
import sys

addrs, names = [], []
for line in open(sys.argv[1], errors='replace'):
    a, _, n = line.rstrip('\n').split(' ', 2)
    addrs.append(int(a, 16))
    names.append(n)
for h in sys.argv[2:]:
    x = int(h, 16)
    i = bisect.bisect_right(addrs, x) - 1
    print(f'{h}\t{names[i][:180]}+{x - addrs[i]:#x}' if i >= 0 else f'{h}\t?')
