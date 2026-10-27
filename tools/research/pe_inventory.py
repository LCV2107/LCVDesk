# Inventario estático de un PE (secciones, entropía, importaciones) sin ejecutarlo. Uso: python -I pe_inventory.py <archivo.exe> <salida.json>
import struct, sys, math, re, json, hashlib

path = sys.argv[1]
data = open(path, 'rb').read()
print('sha256', hashlib.sha256(data).hexdigest(), 'bytes', len(data))
pe = struct.unpack_from('<I', data, 0x3c)[0]
machine, nsec, ts, _, _, optsz, chars = struct.unpack_from('<HHIIIHH', data, pe + 4)
opt = pe + 24
magic = struct.unpack_from('<H', data, opt)[0]
dd_off = opt + (96 if magic == 0x10b else 112)
print('machine', hex(machine), 'timestamp', ts, 'magic', hex(magic))
dirs = [struct.unpack_from('<II', data, dd_off + 8 * i) for i in range(16)]
names = ['export', 'import', 'resource', 'exception', 'security', 'reloc', 'debug', 'arch', 'gptr', 'tls', 'loadcfg', 'bound', 'iat', 'delay', 'clr', 'res']
for n, (r, s) in zip(names, dirs):
    if s:
        print(' dir', n, hex(r), s)
sec_off = opt + optsz
secs = []
def entropy(b):
    if not b:
        return 0
    c = [0] * 256
    for x in b:
        c[x] += 1
    return -sum(v / len(b) * math.log2(v / len(b)) for v in c if v)
for i in range(nsec):
    nm, vs, va, rs, rp = struct.unpack_from('<8sIIII', data, sec_off + 40 * i)
    ch = struct.unpack_from('<I', data, sec_off + 40 * i + 36)[0]
    secs.append((nm.rstrip(b'\0').decode('latin1'), va, vs, rp, rs))
    print(' sec', nm.rstrip(b'\0'), 'va', hex(va), 'vsize', vs, 'raw', rs, 'entropy %.2f' % entropy(data[rp:rp + rs]), 'chars', hex(ch))

def rva2off(r):
    for _, va, vs, rp, rs in secs:
        if va <= r < va + max(vs, rs):
            off = r - va
            return rp + off if off < rs else None
    return None

def cstr(off):
    e = data.index(b'\0', off)
    return data[off:e].decode('latin1')

def walk_imports(rva, delay=False):
    out = {}
    off = rva2off(rva)
    if off is None:
        print(' import dir not file-backed (packed?)')
        return out
    step = 32 if delay else 20
    while True:
        if delay:
            f = struct.unpack_from('<8I', data, off)
            name_rva, int_rva = f[1], f[4]
        else:
            ilt, _, _, name_rva, iat = struct.unpack_from('<5I', data, off)
            int_rva = ilt or iat
        if not name_rva:
            break
        dll = cstr(rva2off(name_rva))
        fns = []
        t = rva2off(int_rva)
        while t is not None:
            v = struct.unpack_from('<I', data, t)[0]
            if not v:
                break
            if v & 0x80000000:
                fns.append('#%d' % (v & 0xffff))
            else:
                o = rva2off(v)
                fns.append(cstr(o + 2) if o is not None else '?')
            t += 4
        out[dll] = fns
        off += step
    return out

imp = walk_imports(dirs[1][0]) if dirs[1][1] else {}
dimp = walk_imports(dirs[13][0], True) if dirs[13][1] else {}
json.dump({'imports': imp, 'delay': dimp}, open(sys.argv[2], 'w'), indent=1)
for k, v in list(imp.items()) + list(dimp.items()):
    print(' DLL', k, len(v), ', '.join(v[:60]))
