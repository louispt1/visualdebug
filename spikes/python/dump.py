import json, sys
for l in open(sys.argv[1]):
    e = json.loads(l)
    loc = " ".join(f"{k}={v['repr'][:50]}" for k, v in (e.get("locals") or {}).items())
    rv = (e.get("return_value") or {}).get("repr")
    print(e["seq"], "  " * e.get("depth", 0), e["event"], "(lib)" if e.get("library") else "",
          f"{e.get('file')}:{e.get('line')}" if e.get("file") else "", e.get("function", ""), loc,
          f"=> {rv[:50]}" if rv else "", e.get("exception", "") or "", "UNWOUND" if e.get("unwound") else "")
