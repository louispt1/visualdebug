const fs = require("fs");
for (const f of process.argv.slice(2)) for (const l of fs.readFileSync(f, "utf8").split("\n").filter(Boolean)) {
  const e = JSON.parse(l);
  const loc = Object.entries(e.locals || {}).map(([k, v]) => `${k}=${v.repr.slice(0, 50)}`).join(" ");
  console.log(e.seq, "  ".repeat(e.depth || 0), e.event, `${e.file}:${e.line}`, e.function, loc, e.return_value ? "=> " + e.return_value.repr : "");
}
