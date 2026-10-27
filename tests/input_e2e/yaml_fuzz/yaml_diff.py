import sys, os, json, subprocess, itertools, yaml, datetime, math, shutil, random

def s(t):
    return json.dumps(t, ensure_ascii=True)

def key(k):
    if isinstance(k, str): return k
    if k is None: return "null"
    if k is True: return "true"
    if k is False: return "false"
    if isinstance(k, int): return str(k)
    if isinstance(k, float):
        if math.isnan(k): return "NaN"
        if math.isinf(k): return "Infinity" if k > 0 else "-Infinity"
        return repr(k)
    if isinstance(k, (datetime.date, datetime.datetime)): return str(k)
    raise yaml.YAMLError("unhashable")

def canon(v):
    if v is None: return "null"
    if v is True: return "true"
    if v is False: return "false"
    if isinstance(v, int): return str(v)
    if isinstance(v, float): return repr(v)
    if isinstance(v, (datetime.date, datetime.datetime)): return "D" + s(str(v))
    if isinstance(v, str): return s(v)
    if isinstance(v, list): return "[" + ",".join(canon(x) for x in v) + "]"
    if isinstance(v, dict): return "{" + ",".join(s(key(k)) + ":" + canon(x) for k, x in v.items()) + "}"
    raise yaml.YAMLError("unsupported " + type(v).__name__)

def py(path):
    try:
        with open(path) as f:
            docs = list(yaml.safe_load_all(f))
        out = " || ".join(canon(d) for d in docs)
        return out if docs else "null"
    except yaml.YAMLError:
        return "ERR_YAML"
    except UnicodeDecodeError:
        return "ERR_READ"
    except Exception:
        return "ERR_VALUE"

def main(corpus_dir, binary):
    files = sorted(os.listdir(corpus_dir))
    paths = [os.path.join(corpus_dir, f) for f in files]
    rs = []
    for i in range(0, len(paths), 500):
        out = subprocess.run([binary] + paths[i:i+500], capture_output=True, text=True).stdout.splitlines()
        rs += out
    bad = 0
    for p, r in zip(paths, rs):
        e = py(p)
        if e != r:
            bad += 1
            print("MISMATCH", p); print("  py:", e[:300]); print("  rs:", r[:300])
            print("  src:", repr(open(p, newline='').read()[:200]))
    print(len(paths), "cases;", bad, "mismatches")

if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
